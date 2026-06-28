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
    last_use: HashMap<usize, usize>,
    /// current instruction position in the block
    pos: usize,
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
            pos: 0,
        }
    }

    /// Pre-compute last-use positions by scanning a block's instructions.
    pub fn build_intervals(&mut self, block: &IrBlock) {
        self.last_use.clear();
        for (i, inst) in block.instrs.iter().enumerate() {
            // Record last-use for each operand
            for op in inst.operands() {
                if let IrOperand::Local(l) = *op {
                    self.last_use.insert(l, i);
                }
            }
        }
        // Remove entries for locals that are only defined (never used)
        // — they can be freed immediately after definition.
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

    /// Allocate a free register. If all are occupied, evicts the one whose
    /// local dies farthest in the future. Returns (register, Option<evicted_local>).
    /// The caller must spill the evicted local if it was dirty.
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
