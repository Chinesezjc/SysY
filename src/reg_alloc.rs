//! Linear-scan register allocator using live intervals.
//!
//! For each basic block we pre-compute the last-use position of every local
//! temporary.  During emission, registers holding locals whose last use has
//! passed are automatically freed.  This gives much better results than the
//! simple "evict-LRU" strategy because we know exactly when a value is dead.

use crate::ir::*;
use std::collections::HashMap;

/// Manages a pool of allocatable RISC-V registers for local temporaries.
pub(crate) struct RegTracker {
    /// local index → (register, dirty?)
    pub locals: HashMap<usize, (String, bool)>,
    /// register → local index
    reg_to_local: HashMap<String, usize>,
    /// Available registers in preference order
    pool: Vec<String>,
    /// last-use position for each local (instruction index within block)
    pub last_use: HashMap<usize, usize>,
    /// first definition position for each local
    first_def: HashMap<usize, usize>,
    /// current instruction position in the block
    pub pos: usize,
    /// maximum number of simultaneously-live pool-register values
    pub max_pool_pressure: usize,
}

impl RegTracker {
    pub fn new() -> Self {
        // t0-t2 for general use.
        // t3 reserved: used as scratch by emit_lw/emit_sw/emit_offset_mul large-offset path.
        let pool = vec!["t2","t1","t0"]
            .iter().map(|s| s.to_string()).collect();
        RegTracker {
            locals: HashMap::new(),
            reg_to_local: HashMap::new(),
            pool,
            last_use: HashMap::new(),
            first_def: HashMap::new(),
            pos: 0,
            max_pool_pressure: 0,
        }
    }

    /// Pre-compute last-use positions and register pressure.
    pub fn build_intervals(&mut self, block: &IrBlock) {
        self.last_use.clear();
        self.first_def.clear();
        for (i, inst) in block.instrs.iter().enumerate() {
            // Record first-def (only if not already set)
            if let Some(dest) = inst.dest() {
                self.first_def.entry(dest).or_insert(i);
            }
            // Record last-use for each operand (overwrites = last one wins)
            for op in inst.operands() {
                if let IrOperand::Local(l) = *op {
                    self.last_use.insert(l, i);
                }
            }
        }
        // Compute max pool pressure: at each position, count locals
        // that are live AND would be allocated from the pool (not params).
        // Pool locals are those defined by Arith/Icmp/GetPtr/GetElemPtr/Load
        // (not those that come from identity-of-param, which use param regs).
        let mut pressure: Vec<usize> = vec![0; block.instrs.len() + 1];
        for (&local, &def) in &self.first_def {
            let end = self.last_use.get(&local).copied().unwrap_or(def);
            // Count only if this local will be allocated from the pool.
            // Identity-of-param locals are NOT counted (they use param regs).
            if self.is_pool_local(block, local) {
                for p in def..=end {
                    if p < pressure.len() { pressure[p] += 1; }
                }
            }
        }
        self.max_pool_pressure = pressure.iter().copied().max().unwrap_or(0);
    }

    /// Determine if a local is pool-allocated (Arith/Icmp/GetPtr/etc. result)
    /// vs param-register-allocated (identity-of-param).
    fn is_pool_local(&self, block: &IrBlock, local: usize) -> bool {
        for inst in &block.instrs {
            if inst.dest() == Some(local) {
                // Identity-of-param (add @param, 0) uses param register, not pool.
                if let IrInst::Arith { op: IrArithOp::Add, rhs: IrOperand::Int(0), lhs, .. } = inst {
                    if matches!(lhs, IrOperand::Global(_)) {
                        return false; // param register, not pool
                    }
                }
                return matches!(inst,
                    IrInst::Arith { .. } | IrInst::Icmp { .. } |
                    IrInst::GetPtr { .. } | IrInst::GetElemPtr { .. } |
                    IrInst::Load { .. } | IrInst::Call { .. }
                );
            }
        }
        false
    }

    /// Advance to instruction position `p`, freeing registers whose
    /// locals are now dead (last use has passed).
    /// Dead locals are simply evicted — their values will never be read
    /// again, so no spill is needed. This eliminates dead stores.
    /// Returns empty vec (no spills needed).
    pub fn advance(&mut self, p: usize) -> Vec<(usize, String)> {
        self.pos = p;
        let dead: Vec<usize> = self.locals.iter()
            .filter(|(l, _)| self.last_use.get(l).map_or(true, |&end| p > end))
            .map(|(l, _)| *l)
            .collect();
        for l in dead {
            self.evict(l);
        }
        Vec::new()
    }

    /// Return the register holding `local`, or None if not cached.
    pub fn get(&self, local: usize) -> Option<String> {
        self.locals.get(&local).map(|(r, _)| r.clone())
    }

    /// Allocate a register. Tries a0 first if free, then pool registers.
    /// Evicts farthest-use if all occupied.
    /// Note: a0 is NOT freed here if occupied — advance() handles that.
    pub fn alloc(&mut self) -> (String, Option<usize>) {
        for reg in &self.pool {
            if !self.reg_to_local.contains_key(reg) {
                return (reg.clone(), None);
            }
        }
        // All occupied — evict the one with farthest next use
        let evict_reg = self.pool.iter()
            .filter_map(|r| self.reg_to_local.get(r).map(|&l| (r.clone(), l)))
            .max_by_key(|(_, l)| self.last_use.get(l).copied().unwrap_or(usize::MAX))
            .map(|(r, _)| r)
            .unwrap_or_else(|| self.pool.last().unwrap().clone());
        let evicted = self.reg_to_local.remove(&evict_reg);
        if let Some(local) = evicted {
            self.locals.remove(&local);
            (evict_reg, Some(local))
        } else {
            (evict_reg, None)
        }
    }

    /// Try to get a0 as destination. Returns None if a0 is occupied by
    /// a live local. Otherwise returns Some((register, evicted_info)).
    pub fn try_alloc_a0(&mut self) -> Option<(String, Option<(usize, bool)>)> {
        if let Some(&occ) = self.reg_to_local.get("a0") {
            let end = *self.last_use.get(&occ).unwrap_or(&usize::MAX);
            if self.pos != end {
                return None; // a0 occupied by live local
            }
            let dirty = self.is_dirty(occ);
            self.locals.remove(&occ);
            self.reg_to_local.remove("a0");
            Some(("a0".to_string(), Some((occ, dirty))))
        } else {
            Some(("a0".to_string(), None)) // a0 is free
        }
    }

    /// Record that `local` is now held in `reg`, and is dirty.
    /// Returns the evicted local index if `reg` was previously occupied.
    pub fn set_dirty(&mut self, local: usize, reg: String) -> Option<usize> {
        let evicted = self.reg_to_local.remove(&reg);
        if let Some(old) = evicted {
            self.locals.remove(&old);
        }
        self.locals.insert(local, (reg.clone(), true));
        self.reg_to_local.insert(reg, local);
        evicted
    }

    /// Mark `local` as clean (value written back to stack).
    pub fn mark_clean(&mut self, local: usize) {
        if let Some(entry) = self.locals.get_mut(&local) {
            entry.1 = false;
        }
    }

    /// Check if `local` is dirty.
    pub fn is_dirty(&self, local: usize) -> bool {
        self.locals.get(&local).map_or(false, |(_, d)| *d)
    }

    /// Return true if `reg` is currently tracked.
    pub fn reg_in_use(&self, reg: &str) -> bool {
        self.reg_to_local.contains_key(reg)
    }

    /// Get the local held in `reg`, if any.
    pub fn local_in_reg(&self, reg: &str) -> Option<usize> {
        self.reg_to_local.get(reg).copied()
    }

    /// Remove a specific local→register mapping.
    pub fn evict(&mut self, local: usize) -> Option<String> {
        if let Some((reg, _)) = self.locals.remove(&local) {
            self.reg_to_local.remove(&reg);
            Some(reg)
        } else {
            None
        }
    }

    /// Remove mapping for a specific register.
    pub fn evict_reg(&mut self, reg: &str) -> Option<usize> {
        if let Some(local) = self.reg_to_local.remove(reg) {
            self.locals.remove(&local);
            Some(local)
        } else {
            None
        }
    }

    /// Flush all dirty locals, returning (local, register) pairs to spill.
    pub fn flush_dirty(&mut self) -> Vec<(usize, String)> {
        let dirty: Vec<(usize, String)> = self.locals.iter()
            .filter(|(_, (_, d))| *d)
            .map(|(l, (r, _))| (*l, r.clone()))
            .collect();
        for (local, _) in &dirty {
            self.evict(*local);
        }
        dirty
    }

    /// Clear all state.
    pub fn clear(&mut self) {
        self.locals.clear();
        self.reg_to_local.clear();
        self.last_use.clear();
        self.pos = 0;
    }
}
