//! Linear-scan register allocator for the RISC-V backend.
//!
//! Tracks which local temporaries are held in registers and spills only
//! when a register is needed for another value.  Works within a single basic
//! block (register state is flushed at block boundaries).

use std::collections::HashMap;

/// Manages a pool of allocatable RISC-V registers for local temporaries.
pub(crate) struct RegTracker {
    /// local index → (register, dirty?)
    locals: HashMap<usize, (String, bool)>,
    /// register → local index
    reg_to_local: HashMap<String, usize>,
    /// Available registers in preference order
    pool: Vec<String>,
}

impl RegTracker {
    pub fn new() -> Self {
        // t0-t4 for general use; t5-t6 reserved for helpers
        let pool = vec!["t4","t3","t2","t1","t0"]
            .iter().map(|s| s.to_string()).collect();
        RegTracker {
            locals: HashMap::new(),
            reg_to_local: HashMap::new(),
            pool,
        }
    }

    /// Return the register holding `local`, or None if not cached.
    pub fn get(&self, local: usize) -> Option<String> {
        self.locals.get(&local).map(|(r, _)| r.clone())
    }

    /// Allocate a free register, spilling the least-preferred occupied one
    /// if needed.  Returns the register name.
    /// Caller must NOT have already marked this register in use — the
    /// returned register is guaranteed free after any spill.
    pub fn alloc(&mut self) -> String {
        for reg in &self.pool {
            if !self.reg_to_local.contains_key(reg) {
                return reg.clone();
            }
        }
        // All pool registers occupied — evict the lowest-priority one
        let evict = self.pool.last().unwrap().clone();
        // Remove the evicted local from tracking (caller must spill it)
        if let Some(local) = self.reg_to_local.remove(&evict) {
            self.locals.remove(&local);
        }
        evict
    }

    /// Record that `local` is now held in `reg`, and is dirty
    /// (not yet written back to its stack slot).
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

    /// Mark `local` as clean (value has been written back to stack).
    /// Keeps the register mapping so subsequent uses can still find it.
    pub fn mark_clean(&mut self, local: usize) {
        if let Some(entry) = self.locals.get_mut(&local) {
            entry.1 = false;
        }
    }

    /// Check if `local` is dirty (register value differs from stack).
    pub fn is_dirty(&self, local: usize) -> bool {
        self.locals.get(&local).map_or(false, |(_, d)| *d)
    }

    /// Return true if `reg` is currently tracked as holding a local.
    pub fn reg_in_use(&self, reg: &str) -> bool {
        self.reg_to_local.contains_key(reg)
    }

    /// Get the local held in `reg`, if any.
    pub fn local_in_reg(&self, reg: &str) -> Option<usize> {
        self.reg_to_local.get(reg).copied()
    }

    /// Remove a specific local→register mapping. Returns the register it was in.
    pub fn evict(&mut self, local: usize) -> Option<String> {
        if let Some((reg, _)) = self.locals.remove(&local) {
            self.reg_to_local.remove(&reg);
            Some(reg)
        } else {
            None
        }
    }

    /// Remove mapping for a specific register. Returns the local it held.
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
            .map(|(&l, (r, _))| (l, r.clone()))
            .collect();
        for (local, _) in &dirty {
            self.evict(*local);
        }
        dirty
    }

    /// Clear all state (for block boundaries).
    pub fn clear(&mut self) {
        self.locals.clear();
        self.reg_to_local.clear();
    }
}
