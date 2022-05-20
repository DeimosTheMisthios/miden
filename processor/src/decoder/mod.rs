use super::{
    ExecutionError, Felt, Join, Loop, OpBatch, Operation, Process, Span, Split, MIN_TRACE_LEN,
};
use vm_core::{FieldElement, Word};

mod trace;
use trace::DecoderTrace;

// DECODER PROCESS EXTENSION
// ================================================================================================

impl Process {
    // JOIN BLOCK
    // --------------------------------------------------------------------------------------------

    pub(super) fn start_join_block(&mut self, block: &Join) -> Result<(), ExecutionError> {
        self.execute_op(Operation::Noop)?;

        let hasher_state = [Felt::ZERO; 12];
        let (addr, _result) = self.hasher.hash(hasher_state);
        self.decoder.start_join(block, addr);

        Ok(())
    }

    pub(super) fn end_join_block(&mut self, block: &Join) -> Result<(), ExecutionError> {
        self.execute_op(Operation::Noop)?;

        self.decoder.end_join(block);

        Ok(())
    }

    // SPLIT BLOCK
    // --------------------------------------------------------------------------------------------

    pub(super) fn start_split_block(&mut self, block: &Split) -> Result<(), ExecutionError> {
        let condition = self.stack.peek();
        self.execute_op(Operation::Drop)?;

        let hasher_state = [Felt::ZERO; 12];
        let (addr, _result) = self.hasher.hash(hasher_state);
        self.decoder.start_split(block, addr, condition);

        Ok(())
    }

    pub(super) fn end_split_block(&mut self, block: &Split) -> Result<(), ExecutionError> {
        self.execute_op(Operation::Noop)?;

        self.decoder.end_split(block);

        Ok(())
    }

    // SPAN BLOCK
    // --------------------------------------------------------------------------------------------

    pub(super) fn start_span_block(&mut self, block: &Span) -> Result<(), ExecutionError> {
        self.execute_op(Operation::Noop)?;

        let first_batch = &block.op_batches()[0].groups();

        let hasher_state = [
            first_batch[0],
            first_batch[1],
            first_batch[2],
            first_batch[3],
            first_batch[4],
            first_batch[5],
            first_batch[6],
            first_batch[7],
            Felt::ZERO,
            Felt::ZERO,
            Felt::ZERO,
            Felt::ZERO,
        ];
        let (addr, _result) = self.hasher.hash(hasher_state);
        self.decoder.start_span(block, addr);

        Ok(())
    }

    pub(super) fn end_span_block(&mut self, block: &Span) -> Result<(), ExecutionError> {
        self.execute_op(Operation::Noop)?;

        self.decoder.end_span(block);

        Ok(())
    }
}

// DECODER
// ================================================================================================
/// TODO: add docs
pub struct Decoder {
    block_stack: BlockStack,
    trace: DecoderTrace,
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            block_stack: BlockStack::new(),
            trace: DecoderTrace::new(),
        }
    }

    // JOIN BLOCK
    // --------------------------------------------------------------------------------------------

    pub fn start_join(&mut self, block: &Join, addr: Felt) {
        let parent_addr = self.block_stack.push(addr);
        let left_child_hash: Word = block.first().hash().into();
        let right_child_hash: Word = block.second().hash().into();
        self.trace.append_row(
            parent_addr,
            Operation::Join,
            left_child_hash,
            right_child_hash,
        );
    }

    pub fn end_join(&mut self, block: &Join) {
        let block_info = self.block_stack.pop();
        let block_hash: Word = block.hash().into();
        self.trace
            .append_row(block_info.addr, Operation::End, block_hash, [Felt::ZERO; 4]);
    }

    // SPLIT BLOCK
    // --------------------------------------------------------------------------------------------

    pub fn start_split(&mut self, block: &Split, addr: Felt, _condition: Felt) {
        let parent_addr = self.block_stack.push(addr);
        let left_child_hash: Word = block.on_true().hash().into();
        let right_child_hash: Word = block.on_false().hash().into();
        self.trace.append_row(
            parent_addr,
            Operation::Split,
            left_child_hash,
            right_child_hash,
        );
    }

    pub fn end_split(&mut self, block: &Split) {
        let block_info = self.block_stack.pop();
        let block_hash: Word = block.hash().into();
        self.trace
            .append_row(block_info.addr, Operation::End, block_hash, [Felt::ZERO; 4]);
    }

    // LOOP BLOCK
    // --------------------------------------------------------------------------------------------

    pub fn start_loop(&mut self, _block: &Loop, _condition: Felt) {
        // TODO: implement
    }

    pub fn repeat(&mut self, _block: &Loop) {
        // TODO: implement
    }

    pub fn end_loop(&mut self, _block: &Loop) {
        // TODO: implement
    }

    // SPAN BLOCK
    // --------------------------------------------------------------------------------------------
    pub fn start_span(&mut self, block: &Span, addr: Felt) {
        let parent_addr = self.block_stack.push(addr);
        let first_op_batch = &block.op_batches()[0].groups();
        let num_op_groups = get_group_count(block);
        self.trace
            .append_span_start(parent_addr, addr, first_op_batch, num_op_groups);
    }

    pub fn respan(&mut self, op_batch: &OpBatch) {
        self.trace.append_respan(op_batch.groups());
    }

    pub fn execute_user_op(&mut self, op: Operation, num_groups_left: Felt, group_ops_left: Felt) {
        debug_assert!(!op.is_decorator(), "op is a decorator");
        self.trace
            .append_user_op(op, num_groups_left, group_ops_left);
    }

    pub fn end_span(&mut self, block: &Span) {
        let _block_info = self.block_stack.pop();
        let block_hash: Word = block.hash().into();
        self.trace.append_span_end(block_hash, Felt::ZERO);
    }

    // TRACE GENERATIONS
    // --------------------------------------------------------------------------------------------

    /// TODO: add docs
    pub fn into_trace(self, trace_len: usize, num_rand_rows: usize) -> super::DecoderTrace {
        self.trace
            .into_vec(trace_len, num_rand_rows)
            .try_into()
            .expect("failed to convert vector to array")
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

// HELPER FUNCTIONS
// ================================================================================================

// TODO: move to assembler
fn get_group_count(block: &Span) -> Felt {
    let mut result = 0;
    for batch in block.op_batches() {
        result += batch.num_groups() as u64;
    }
    Felt::new(result)
}

// BLOCK INFO
// ================================================================================================

pub struct BlockStack {
    blocks: Vec<BlockInfo>,
}

impl BlockStack {
    pub fn new() -> Self {
        Self { blocks: Vec::new() }
    }

    pub fn push(&mut self, addr: Felt) -> Felt {
        let parent_addr = if self.blocks.is_empty() {
            Felt::ZERO
        } else {
            self.blocks[self.blocks.len() - 1].addr
        };
        self.blocks.push(BlockInfo { addr, parent_addr });

        parent_addr
    }

    pub fn pop(&mut self) -> BlockInfo {
        self.blocks.pop().expect("block stack is empty")
    }

    #[allow(dead_code)]
    pub fn peek_addr(&self) -> Felt {
        self.blocks.last().expect("block stack is empty").addr
    }
}

#[allow(dead_code)]
pub struct BlockInfo {
    addr: Felt,
    parent_addr: Felt,
}
