use super::{
    BindingKind, EvalError, EvalScratch, ExternalStrings, Operand, Stack, StringPools,
    TransientStrings, Update, UpdateResult, Value, push_operand,
    realm::{Artifact, FunctionOrigin},
    update_number,
};

pub(super) struct UpdateNumeric<'pools, 'budget> {
    pub(super) source: TransientStrings<'pools>,
    pub(super) credits: &'budget mut usize,
}

// Only binding instructions enter this policy. The dispatcher and all
// coercions, control, and accounting remain shared.
pub(super) trait Bindings {
    type Error: From<EvalError>;
    const CALLS: bool = false;

    fn retained_units(&self) -> &[u16] {
        &[]
    }

    fn artifacts(&self) -> &[Artifact] {
        &[]
    }

    fn active_artifact(&self) -> Option<u8> {
        None
    }

    fn select_body(&mut self, _: Option<FunctionOrigin>) {}

    fn non_callable(&self, _: crate::Span) -> Self::Error {
        EvalError::InvalidScript.into()
    }

    fn charge_call(&mut self, _: usize) -> Result<(), Self::Error> {
        Ok(())
    }

    fn transient<'a>(
        &self,
        scratch: &'a EvalScratch,
        literals: &'a [u16],
        artifacts: &'a [Artifact],
    ) -> TransientStrings<'a> {
        let mut source = scratch.transient(literals);
        source.artifacts = artifacts;
        source
    }

    fn external_strings<'a>(&'a self, literals: &'a [u16]) -> ExternalStrings<'a> {
        ExternalStrings {
            literals,
            artifacts: self.artifacts(),
            active_artifact: self.active_artifact(),
            retained: self.retained_units(),
        }
    }

    fn string_pools<'a>(
        &'a self,
        scratch: &'a EvalScratch,
        literals: &'a [u16],
    ) -> StringPools<'a> {
        self.transient(scratch, literals, self.artifacts())
            .with_retained(self.retained_units())
    }

    fn load(&mut self, slot: usize) -> Result<Operand, Self::Error>;
    fn init(
        &mut self,
        slot: usize,
        kind: BindingKind,
        ip: usize,
        stack: &mut Stack<'_>,
        source: TransientStrings<'_>,
    ) -> Result<(), Self::Error>;
    fn store(
        &mut self,
        slot: usize,
        keep: bool,
        stack: &mut Stack<'_>,
        source: TransientStrings<'_>,
    ) -> Result<(), Self::Error>;
    fn update(
        &mut self,
        slot: usize,
        op: Update,
        result: UpdateResult,
        stack: &mut Stack<'_>,
        max: usize,
        numeric: UpdateNumeric<'_, '_>,
    ) -> Result<(), Self::Error>;
}

pub(super) struct LegacyBindings(Vec<Option<Slot>>);

struct Slot {
    init_at: usize,
    kind: BindingKind,
    value: Operand,
}

impl LegacyBindings {
    pub(super) fn new(count: usize) -> Result<Self, EvalError> {
        // Each declaration has its own slot, local to this evaluation. Slots
        // in untaken paths may remain empty. Only an initializer can reset a
        // const. No clearing on scope exit is needed: the parser references
        // slots only in their scope after the initializer, and jumps cannot
        // enter a suffix from outside the block.
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(count)
            .map_err(|_| EvalError::AllocationFailed)?;
        slots.resize_with(count, || None);
        Ok(Self(slots))
    }

    fn writable(&mut self, slot: usize) -> Result<&mut Operand, EvalError> {
        match self.0.get_mut(slot) {
            Some(Some(Slot {
                kind: BindingKind::Let,
                value,
                ..
            })) => Ok(value),
            _ => Err(EvalError::InvalidScript),
        }
    }
}

impl Bindings for LegacyBindings {
    type Error = EvalError;

    fn load(&mut self, slot: usize) -> Result<Operand, EvalError> {
        self.0
            .get(slot)
            .and_then(Option::as_ref)
            .map(|slot| slot.value)
            .ok_or(EvalError::InvalidScript)
    }

    fn init(
        &mut self,
        slot: usize,
        kind: BindingKind,
        init_at: usize,
        stack: &mut Stack<'_>,
        _: TransientStrings<'_>,
    ) -> Result<(), EvalError> {
        let binding = self.0.get_mut(slot).ok_or(EvalError::InvalidScript)?;
        if binding.as_ref().is_some_and(|old| old.init_at != init_at) {
            return Err(EvalError::InvalidScript);
        }
        *binding = Some(Slot {
            init_at,
            kind,
            value: init_operand(stack)?,
        });
        Ok(())
    }

    fn store(
        &mut self,
        slot: usize,
        keep: bool,
        stack: &mut Stack<'_>,
        _: TransientStrings<'_>,
    ) -> Result<(), EvalError> {
        let value = self.writable(slot)?;
        *value = store_operand(stack, keep)?;
        Ok(())
    }

    fn update(
        &mut self,
        slot: usize,
        op: Update,
        result: UpdateResult,
        stack: &mut Stack<'_>,
        max: usize,
        numeric: UpdateNumeric<'_, '_>,
    ) -> Result<(), EvalError> {
        let value = self.writable(slot)?;
        let old = value.to_number(numeric.source.with_retained(&[]), numeric.credits)?;
        let new = update_number(op, old);
        update_result(stack, max, result, old, new)?;
        *value = Operand::Scalar(Value::Number(new));
        Ok(())
    }
}

pub(super) fn init_operand(stack: &mut Stack<'_>) -> Result<Operand, EvalError> {
    if stack.len() != 1 {
        return Err(EvalError::InvalidScript);
    }
    stack.pop().ok_or(EvalError::InvalidScript)
}

pub(super) fn store_operand(stack: &mut Stack<'_>, keep: bool) -> Result<Operand, EvalError> {
    if keep {
        stack.last().copied().ok_or(EvalError::InvalidScript)
    } else {
        init_operand(stack)
    }
}

pub(super) fn update_result(
    stack: &mut Stack<'_>,
    max: usize,
    result: UpdateResult,
    old: f64,
    new: f64,
) -> Result<(), EvalError> {
    let result = match result {
        UpdateResult::New => new,
        UpdateResult::Old => old,
    };
    push_operand(stack, max, Operand::Scalar(Value::Number(result)))
}
