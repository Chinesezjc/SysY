//! Control flow graph construction for [`IrFunc`].

use crate::ir::*;
use std::collections::HashMap;

/// CFG for a single function.
pub struct Cfg {
    /// Maps block label index → position in the function's block list.
    pub block_index: HashMap<usize, usize>,
    /// Successors[block_pos] = list of successor block positions.
    pub successors: Vec<Vec<usize>>,
    /// Predecessors[block_pos] = list of predecessor block positions.
    pub predecessors: Vec<Vec<usize>>,
}

impl Cfg {
    /// Build the CFG for a function.  Fills `preds` on each block as a side
    /// effect.
    pub fn build(func: &mut IrFunc) -> Self {
        let n = func.blocks.len();

        // Build label → position map
        let mut block_index: HashMap<usize, usize> = HashMap::new();
        for (i, block) in func.blocks.iter().enumerate() {
            block_index.insert(block.label, i);
        }

        let mut successors: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut predecessors: Vec<Vec<usize>> = vec![Vec::new(); n];

        for (i, block) in func.blocks.iter().enumerate() {
            // Clear existing preds (will be repopulated)
            // (actual mutation happens below)

            // Find terminator
            if let Some(last) = block.instrs.last() {
                match last {
                    IrInst::Br {
                        then_bb, else_bb, ..
                    } => {
                        if let Some(&t) = block_index.get(then_bb) {
                            successors[i].push(t);
                            predecessors[t].push(i);
                        }
                        if let Some(&e) = block_index.get(else_bb) {
                            successors[i].push(e);
                            predecessors[e].push(i);
                        }
                    }
                    IrInst::Jump { target } => {
                        if let Some(&t) = block_index.get(target) {
                            successors[i].push(t);
                            predecessors[t].push(i);
                        }
                    }
                    IrInst::Ret { .. } => {
                        // No successors
                    }
                    _ => {
                        // Non-terminator at end — should not happen in valid IR
                    }
                }
            }
        }

        // Write predecessors back to blocks (collect labels first to avoid borrow conflict)
        let pred_labels: Vec<Vec<usize>> = predecessors
            .iter()
            .map(|preds| preds.iter().map(|&p| func.blocks[p].label).collect())
            .collect();
        for (i, block) in func.blocks.iter_mut().enumerate() {
            block.preds = pred_labels[i].clone();
        }

        Cfg {
            block_index,
            successors,
            predecessors,
        }
    }

    /// Number of basic blocks.
    pub fn len(&self) -> usize {
        self.successors.len()
    }

    /// Returns true if the CFG is empty.
    pub fn is_empty(&self) -> bool {
        self.successors.is_empty()
    }
}
