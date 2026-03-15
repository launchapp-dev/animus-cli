use crate::state_machines::{builtin_compiled_state_machines, CompiledWorkflowMachine, TransitionError};
use crate::types::{WorkflowMachineEvent, WorkflowMachineState};

#[derive(Debug, Clone)]
pub struct WorkflowStateMachine {
    current: WorkflowMachineState,
    definition: CompiledWorkflowMachine,
}

impl Default for WorkflowStateMachine {
    fn default() -> Self {
        let compiled = builtin_compiled_state_machines();
        Self::with_definition(WorkflowMachineState::Idle, compiled.workflow)
    }
}

impl WorkflowStateMachine {
    pub fn new(initial: WorkflowMachineState) -> Self {
        let compiled = builtin_compiled_state_machines();
        Self::with_definition(initial, compiled.workflow)
    }

    pub fn with_definition(initial: WorkflowMachineState, definition: CompiledWorkflowMachine) -> Self {
        Self { current: initial, definition }
    }

    pub fn state(&self) -> WorkflowMachineState {
        self.current
    }

    pub fn apply(&mut self, event: WorkflowMachineEvent) -> Result<WorkflowMachineState, TransitionError> {
        let outcome = self.definition.apply(self.current, event, |_| true)?;
        self.current = outcome.to;
        Ok(self.current)
    }
}
