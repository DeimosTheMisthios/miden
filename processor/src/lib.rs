use vm_core::{
    errors::AdviceSetError,
    hasher::Digest,
    program::{
        blocks::{CodeBlock, Join, Loop, OpBatch, Span, Split, OP_GROUP_SIZE},
        Script,
    },
    AdviceInjector, DebugOptions, Felt, FieldElement, Operation, ProgramInputs, StackTopState,
    StarkField, Word, AUX_TRACE_WIDTH, MIN_STACK_DEPTH, MIN_TRACE_LEN, NUM_STACK_HELPER_COLS,
    RANGE_CHECK_TRACE_WIDTH, STACK_TRACE_WIDTH, SYS_TRACE_WIDTH,
};

mod operations;

mod system;
use system::System;
pub use system::FMP_MIN;

mod decoder;
use decoder::Decoder;

mod stack;
use stack::Stack;

mod range;
use range::RangeChecker;

mod hasher;
use hasher::Hasher;

mod bitwise;
use bitwise::Bitwise;

mod memory;
use memory::Memory;

mod advice;
use advice::AdviceProvider;

mod aux_table;
use aux_table::AuxTable;

mod trace;
pub use trace::ExecutionTrace;
use trace::TraceFragment;

mod errors;
pub use errors::ExecutionError;

mod debug;
pub use debug::{VmState, VmStateIterator};

// TYPE ALIASES
// ================================================================================================

type SysTrace = [Vec<Felt>; SYS_TRACE_WIDTH];
type StackTrace = [Vec<Felt>; STACK_TRACE_WIDTH];
type RangeCheckTrace = [Vec<Felt>; RANGE_CHECK_TRACE_WIDTH];
type AuxTableTrace = [Vec<Felt>; AUX_TRACE_WIDTH]; // TODO: potentially rename to AuxiliaryTrace

// EXECUTOR
// ================================================================================================

/// Returns an execution trace resulting from executing the provided script against the provided
/// inputs.
pub fn execute(script: &Script, inputs: &ProgramInputs) -> Result<ExecutionTrace, ExecutionError> {
    let mut process = Process::new(inputs.clone());
    process.execute_code_block(script.root())?;
    // TODO: make sure program hash from script and trace are the same
    Ok(ExecutionTrace::new(process, *script.hash()))
}

/// Returns an iterator that allows callers to step through each exceution and inspect
/// vm state information along side.
pub fn execute_iter(script: &Script, inputs: &ProgramInputs) -> VmStateIterator {
    let mut process = Process::new_debug(inputs.clone());
    let result = process.execute_code_block(script.root());
    VmStateIterator::new(process, result)
}

// PROCESS
// ================================================================================================

pub struct Process {
    system: System,
    decoder: Decoder,
    stack: Stack,
    range: RangeChecker,
    hasher: Hasher,
    bitwise: Bitwise,
    memory: Memory,
    advice: AdviceProvider,
}

impl Process {
    fn initialize(inputs: ProgramInputs, keep_overflow_trace: bool) -> Self {
        Self {
            system: System::new(MIN_TRACE_LEN),
            decoder: Decoder::new(),
            stack: Stack::new(&inputs, MIN_TRACE_LEN, keep_overflow_trace),
            range: RangeChecker::new(),
            hasher: Hasher::new(),
            bitwise: Bitwise::new(),
            memory: Memory::new(),
            advice: AdviceProvider::new(inputs),
        }
    }

    /// Creates a new process with the provided inputs.
    pub fn new(inputs: ProgramInputs) -> Self {
        Self::initialize(inputs, false)
    }

    /// Creates a new process with provided inputs and debug options enabled.
    pub fn new_debug(inputs: ProgramInputs) -> Self {
        Self::initialize(inputs, true)
    }

    // CODE BLOCK EXECUTORS
    // --------------------------------------------------------------------------------------------

    /// Executes the specified [CodeBlock].
    ///
    /// # Errors
    /// Returns an [ExecutionError] if executing the specified block fails for any reason.
    pub fn execute_code_block(&mut self, block: &CodeBlock) -> Result<(), ExecutionError> {
        match block {
            CodeBlock::Join(block) => self.execute_join_block(block),
            CodeBlock::Split(block) => self.execute_split_block(block),
            CodeBlock::Loop(block) => self.execute_loop_block(block),
            CodeBlock::Span(block) => self.execute_span_block(block),
            CodeBlock::Proxy(_) => Err(ExecutionError::UnexecutableCodeBlock(block.clone())),
            _ => Err(ExecutionError::UnsupportedCodeBlock(block.clone())),
        }
    }

    /// Executes the specified [Join] block.
    #[inline(always)]
    fn execute_join_block(&mut self, block: &Join) -> Result<(), ExecutionError> {
        self.start_join_block(block)?;

        // execute first and then second child of the join block
        self.execute_code_block(block.first())?;
        self.execute_code_block(block.second())?;

        self.end_join_block(block)
    }

    /// Executes the specified [Split] block.
    #[inline(always)]
    fn execute_split_block(&mut self, block: &Split) -> Result<(), ExecutionError> {
        // get the top element from the stack here because starting a SPLIT block removes it from
        // the stack
        let condition = self.stack.peek();
        self.start_split_block(block)?;

        // execute either the true or the false branch of the split block based on the condition
        // retrieved from the top of the stack
        if condition == Felt::ONE {
            self.execute_code_block(block.on_true())?;
        } else if condition == Felt::ZERO {
            self.execute_code_block(block.on_false())?;
        } else {
            return Err(ExecutionError::NotBinaryValue(condition));
        }

        self.end_split_block(block)
    }

    /// Executes the specified [Loop] block.
    #[inline(always)]
    fn execute_loop_block(&mut self, block: &Loop) -> Result<(), ExecutionError> {
        // start LOOP block; this requires examining the top of the stack to determine whether
        // the loop's body should be executed.
        let condition = self.stack.peek();
        self.decoder.start_loop(block, condition);

        // if the top of the stack is ONE, execute the loop body; otherwise skip the loop body;
        // before we execute the loop body we drop the condition from the stack; when the loop
        // body is not executed, we keep the condition on the stack so that it can be dropped by
        // the END operation later.
        if condition == Felt::ONE {
            // drop the condition and execute the loop body at least once
            self.execute_op(Operation::Drop)?;
            self.execute_code_block(block.body())?;

            // keep executing the loop body until the condition on the top of the stack is no
            // longer ONE; each iteration of the loop is preceded by executing REPEAT operation
            // which drops the condition from the stack
            while self.stack.peek() == Felt::ONE {
                self.execute_op(Operation::Drop)?;
                self.decoder.repeat(block);

                self.execute_code_block(block.body())?;
            }
        } else if condition == Felt::ZERO {
            self.execute_op(Operation::Noop)?
        } else {
            return Err(ExecutionError::NotBinaryValue(condition));
        }

        // execute END operation; this can be done only if the top of the stack is ZERO, in which
        // case the top of the stack is dropped
        if self.stack.peek() == Felt::ZERO {
            self.execute_op(Operation::Drop)?;
        } else if condition == Felt::ONE {
            unreachable!("top of the stack should not be ONE");
        } else {
            return Err(ExecutionError::NotBinaryValue(self.stack.peek()));
        }
        self.decoder.end_loop(block);

        Ok(())
    }

    /// Executes the specified [Span] block.
    #[inline(always)]
    fn execute_span_block(&mut self, block: &Span) -> Result<(), ExecutionError> {
        self.start_span_block(block)?;

        // execute the first operation batch
        self.execute_op_batch(&block.op_batches()[0])?;

        // if the span contains more operation batches, execute them. each additional batch is
        // preceded by a RESPAN operation; executing RESPAN operation does not change the state
        // of the stack
        for op_batch in block.op_batches().iter().skip(1) {
            self.decoder.respan(op_batch);
            self.execute_op(Operation::Noop)?;
            self.execute_op_batch(op_batch)?;
        }

        self.end_span_block(block)
    }

    /// Executes all operations in an [OpBatch]. This also ensure that all alignment rules are
    /// satisfied by executing NOOPs as needed. Specifically:
    /// - If an operation group ends with an operation carrying an immediate value, a NOOP is
    ///   executed after it.
    /// - If the number of groups in a batch is not a power of 2, NOOPs are executed (one per
    ///   group) to bring it up to the next power of two (e.g., 3 -> 4, 5 -> 8).
    #[inline(always)]
    fn execute_op_batch(&mut self, batch: &OpBatch) -> Result<(), ExecutionError> {
        let op_counts = batch.op_counts();
        let mut op_idx = 0;
        let mut group_idx = 0;
        let mut next_group_idx = 1;

        // execute operations in the batch one by one
        for &op in batch.ops() {
            if op.is_decorator() {
                // if the operation is a decorator, it has no side effects and, thus, we don't
                // need to decode it
                self.execute_op(op)?;
                continue;
            }

            // decode and execute the operation
            self.decoder.execute_user_op(op);
            self.execute_op(op)?;

            // if the operation carries an immediate value, the value is stored at the next group
            // pointer; so, we advance the pointer to the following group
            let has_imm = op.imm_value().is_some();
            if has_imm {
                next_group_idx += 1;
            }

            // determine if we've executed all non-decorator operations in a group
            if op_idx == op_counts[group_idx] - 1 {
                // if so, move to the next group and reset operation index
                group_idx = next_group_idx;
                next_group_idx += 1;
                op_idx = 0;

                // if the operation carries immediate value, it cannot be the last operation in
                // the group - so, just execute a NOOP after it
                if has_imm {
                    // an operation with immediate value cannot be in the 9th position
                    debug_assert!(op_idx < OP_GROUP_SIZE - 1, "invalid op index");
                    self.decoder.execute_user_op(Operation::Noop);
                    self.execute_op(Operation::Noop)?;
                }
            } else {
                // if we are not at the end of the group, just increment the operation index
                op_idx += 1;
            }
        }

        // for each operation batch we must execute number of groups which is a power of 2; so
        // if the number of groups is not a power of 2, we pad each missing group with a NOOP
        for _ in group_idx..batch.num_groups().next_power_of_two() {
            self.decoder.execute_user_op(Operation::Noop);
            self.execute_op(Operation::Noop)?;
        }

        Ok(())
    }

    // PUBLIC ACCESSORS
    // --------------------------------------------------------------------------------------------
    pub fn get_memory_value(&self, addr: u64) -> Option<Word> {
        self.memory.get_value(addr)
    }

    pub fn to_components(self) -> (System, Stack, RangeChecker, AuxTable) {
        let aux_table = AuxTable::new(self.hasher, self.bitwise, self.memory);
        (self.system, self.stack, self.range, aux_table)
    }
}
