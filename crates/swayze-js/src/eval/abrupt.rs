//! Private guarded dispatch. Gating and diagnostics belong to the binding policy.
use super::*;
use crate::{Span, parser::CatchSlot};

pub(super) const MAX_REGIONS: usize = 128;

#[derive(Clone, Copy)]
pub(super) struct Thrown {
    pub(super) value: Operand,
    pub(super) owner: Option<realm::FunctionOrigin>,
    pub(super) span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Protected,
    Active,
}

struct Region {
    value: Operand,
    enter: u16,
    slots: u16,
    packets: u16,
    depth: u8,
    receivers: u8,
    phase: Phase,
}
const _: () = assert!(std::mem::size_of::<Region>() <= 32);

#[derive(Default)]
pub(super) struct Regions(Vec<Region>);

pub(super) struct Unwind<'a, 'code> {
    pub(super) frame: &'a mut Frame<'code>,
    pub(super) callers: &'a mut Vec<Frame<'code>>,
    pub(super) slots: &'a mut Vec<Operand>,
    pub(super) packets: &'a mut Packets,
    pub(super) receivers: &'a mut usize,
    pub(super) fuel: &'a mut usize,
}

pub(super) fn debit(fuel: &mut usize, work: usize) -> Result<(), EvalError> {
    *fuel = fuel
        .checked_sub(work)
        .ok_or(EvalError::DispatchLimitExceeded)?;
    Ok(())
}

fn bounds(ops: &[Instruction], enter: usize) -> Result<(usize, usize), EvalError> {
    let Some(&Instruction::EnterTry { catch, after }) = ops.get(enter) else {
        return Err(EvalError::InvalidScript);
    };
    let (c, a) = (usize::from(catch), usize::from(after));
    let enter = enter as u16;
    if usize::from(enter) + 2 > c
        || c + 2 > a
        || a > ops.len()
        || ops[c - 1] != (Instruction::LeaveTry { enter })
        || ops[c] != (Instruction::EnterCatch { enter })
        || ops[a - 1] != (Instruction::LeaveCatch { enter })
    {
        return Err(EvalError::InvalidScript);
    }
    Ok((c, a))
}

pub(super) fn guarded(op: Instruction) -> bool {
    matches!(
        op,
        Instruction::EnterTry { .. }
            | Instruction::LeaveTry { .. }
            | Instruction::EnterCatch { .. }
            | Instruction::LeaveCatch { .. }
            | Instruction::ThrowValue { .. }
            | Instruction::LoadCatch(_)
            | Instruction::StoreCatch { .. }
            | Instruction::UpdateCatch { .. }
            | Instruction::ExitRegions { .. }
    )
}

impl Regions {
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    pub(super) fn enter(
        &mut self,
        enter: usize,
        frame: &Frame<'_>,
        depth: usize,
        receivers: usize,
        fuel: &mut usize,
    ) -> Result<(), EvalError> {
        bounds(frame.ops, enter)?;
        if self.len() == MAX_REGIONS {
            return Err(EvalError::LiveSlotLimitExceeded);
        }
        check_live_slots(
            usize::from(frame.temps),
            usize::from(frame.max_operands),
            usize::from(frame.fence) + self.len() + 1,
            receivers,
        )?;
        let cold = self.0.capacity() == 0;
        debit(fuel, 1 + if cold { MAX_REGIONS } else { 0 })?;
        if cold {
            reserve_regions(&mut self.0)?;
        }
        self.0.push(Region {
            value: UNDEFINED,
            enter: enter as u16,
            slots: frame.temps,
            packets: frame.fence,
            depth: depth as u8,
            receivers: receivers as u8,
            phase: Phase::Protected,
        });
        Ok(())
    }

    fn local(&self, frame: &Frame<'_>) -> Result<&[Region], EvalError> {
        self.0
            .get(usize::from(frame.region_fence)..)
            .ok_or(EvalError::InvalidScript)
    }

    pub(super) fn boundary(
        &self,
        frame: &Frame<'_>,
        depth: usize,
        enter: u16,
        at: usize,
        op: Instruction,
    ) -> Result<usize, EvalError> {
        let row = self.local(frame)?.last().ok_or(EvalError::InvalidScript)?;
        let (catch, after) = bounds(frame.ops, usize::from(enter))?;
        let (phase, expected) = match op {
            Instruction::LeaveTry { .. } => (Phase::Protected, catch - 1),
            Instruction::EnterCatch { .. } => (Phase::Active, catch),
            Instruction::LeaveCatch { .. } => (Phase::Active, after - 1),
            _ => return Err(EvalError::InvalidScript),
        };
        if row.enter != enter
            || usize::from(row.depth) != depth
            || row.phase != phase
            || at != expected
        {
            return Err(EvalError::InvalidScript);
        }
        Ok(after)
    }

    pub(super) fn cell(
        &mut self,
        frame: &Frame<'_>,
        depth: usize,
        at: usize,
        slot: CatchSlot,
    ) -> Result<&mut Operand, EvalError> {
        let index = usize::from(frame.region_fence)
            + usize::from(slot.depth)
                .checked_sub(1)
                .ok_or(EvalError::InvalidScript)?;
        let row = self.0.get_mut(index).ok_or(EvalError::InvalidScript)?;
        let (c, a) = bounds(frame.ops, usize::from(slot.enter))?;
        if row.enter != slot.enter
            || usize::from(row.depth) != depth
            || row.phase != Phase::Active
            || !(c + 1..a - 1).contains(&at)
        {
            return Err(EvalError::InvalidScript);
        }
        Ok(&mut row.value)
    }

    pub(super) fn drop_local(
        &mut self,
        fence: u8,
        pop: usize,
        fuel: &mut usize,
    ) -> Result<(), EvalError> {
        if pop
            > self
                .len()
                .checked_sub(usize::from(fence))
                .ok_or(EvalError::InvalidScript)?
        {
            return Err(EvalError::InvalidScript);
        }
        debit(fuel, pop)?;
        self.0.truncate(self.len() - pop);
        Ok(())
    }

    pub(super) fn exit(
        &mut self,
        frame: &Frame<'_>,
        depth: usize,
        op: Instruction,
        fuel: &mut usize,
    ) -> Result<(), EvalError> {
        let Instruction::ExitRegions {
            target,
            pop,
            backward,
        } = op
        else {
            return Err(EvalError::InvalidScript);
        };
        let at = frame.ip.checked_sub(1).ok_or(EvalError::InvalidScript)?;
        let (target, pop) = (usize::from(target), usize::from(pop));
        let local = self.local(frame)?;
        if pop == 0
            || pop > local.len()
            || target > frame.ops.len()
            || (if backward { target >= at } else { target <= at })
            || matches!(frame.ops.get(target), Some(Instruction::EnterCatch { .. }))
        {
            return Err(EvalError::InvalidScript);
        }
        let keep = local.len() - pop;
        // Validate the boundary of the suffix; all interior rows were entered
        // and authenticated in this activation. Never inspect discarded code.
        let interval = |row: &Region| -> Result<std::ops::Range<usize>, EvalError> {
            if usize::from(row.depth) != depth {
                return Err(EvalError::InvalidScript);
            }
            let (c, a) = bounds(frame.ops, usize::from(row.enter))?;
            Ok(if row.phase == Phase::Protected {
                usize::from(row.enter) + 1..c
            } else {
                c..a
            })
        };
        let exited = interval(&local[keep])?;
        if !exited.contains(&at)
            || exited.contains(&target)
            || (keep != 0 && !interval(&local[keep - 1])?.contains(&target))
        {
            return Err(EvalError::InvalidScript);
        }
        self.drop_local(frame.region_fence, pop, fuel)
    }

    pub(super) fn transfer<'code, B: Bindings>(
        &mut self,
        thrown: Thrown,
        state: Unwind<'_, 'code>,
        bindings: &mut B,
    ) -> Result<bool, EvalError> {
        let Unwind {
            frame,
            callers,
            slots,
            packets,
            receivers,
            fuel,
        } = state;
        let mut visited = 0;
        let selected = self.0.iter().rposition(|r| {
            visited += 1;
            r.phase == Phase::Protected
        });
        let Some(index) = selected else {
            debit(fuel, visited)?;
            return Ok(false);
        };
        let r = &self.0[index];
        let depth = usize::from(r.depth);
        let old_depth = callers.len();
        let catcher = if depth == old_depth {
            &*frame
        } else {
            callers.get(depth).ok_or(EvalError::InvalidScript)?
        };
        let enter = usize::from(r.enter);
        let (catch, _) = bounds(catcher.ops, enter)?;
        let interrupted = catcher.ip.checked_sub(1).ok_or(EvalError::InvalidScript)?;
        if !(enter + 1..catch - 1).contains(&interrupted) {
            return Err(EvalError::InvalidScript);
        }
        let (s, p, rs) = (
            usize::from(r.slots),
            usize::from(r.packets),
            usize::from(r.receivers),
        );
        if index < usize::from(catcher.region_fence)
            || s != usize::from(catcher.temps)
            || p != usize::from(catcher.fence)
            || s > slots.len()
            || p > packets.rows.len()
            || rs > *receivers
            || (depth == old_depth && rs != *receivers)
        {
            return Err(EvalError::InvalidScript);
        }
        let dropped_frames = old_depth - depth;
        debit(
            fuel,
            visited + self.len() - index - 1 + 2 * dropped_frames + 1,
        )?;
        if dropped_frames != 0 {
            let discarded_receivers = usize::from(frame.receiver.is_some())
                + callers[depth + 1..]
                    .iter()
                    .filter(|f| f.receiver.is_some())
                    .count();
            if receivers.checked_sub(discarded_receivers) != Some(rs) {
                return Err(EvalError::InvalidScript);
            }
            callers.truncate(depth + 1);
            *frame = callers.pop().ok_or(EvalError::InvalidScript)?;
        }
        slots.truncate(s);
        packets.rows.truncate(p);
        *receivers = rs;
        self.0.truncate(index + 1);
        self.0[index].phase = Phase::Active;
        self.0[index].value = thrown.value;
        frame.ip = catch;
        bindings.select_body(frame.body);
        Ok(true)
    }
}

fn reserve_regions(rows: &mut Vec<Region>) -> Result<(), EvalError> {
    #[cfg(test)]
    if FAIL_RESERVE.with(|flag| flag.replace(false)) {
        return Err(EvalError::AllocationFailed);
    }
    rows.try_reserve_exact(MAX_REGIONS)
        .map_err(|_| EvalError::AllocationFailed)
}

#[cfg(test)]
thread_local! { pub(super) static FAIL_RESERVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

const ACTIVE: u16 = 0x8000;
const UNSEEDED: u16 = u16::MAX;

#[derive(Clone, Copy)]
struct Shape {
    arm: u16,
    packet: u16,
    height: u16,
    depth: u8,
}
const _: () = assert!(std::mem::size_of::<Shape>() == 8);

fn opener(arm: u16) -> Result<usize, EvalError> {
    usize::from(arm & !ACTIVE)
        .checked_sub(1)
        .ok_or(EvalError::InvalidScript)
}

pub(super) fn validate<E: From<EvalError>>(
    ops: &[Instruction],
    peak: usize,
    exit: ExitStack,
    source_len: u32,
    locals: Option<usize>,
    mut charge: impl FnMut(usize) -> Result<(), E>,
    mut check_site: impl FnMut(u16) -> Result<(), E>,
) -> Result<(), E> {
    use Instruction::*;
    let bad = || E::from(EvalError::InvalidScript);
    validate_body(ops, peak, exit)?;
    charge(ops.len() + 1)?;
    let mut shape = Vec::<Shape>::new();
    shape
        .try_reserve_exact(ops.len() + 1)
        .map_err(|_| EvalError::AllocationFailed)?;
    let (mut arm, mut packet, mut depth) = (0, 0, 0);
    for (at, &op) in ops.iter().enumerate() {
        charge(1)?;
        shape.push(Shape {
            arm,
            packet,
            height: UNSEEDED,
            depth,
        });
        if packet != 0 && ends_packet_scope(op) {
            return Err(bad());
        }
        packet = packet_shape(ops, at, packet, |p| shape[p].packet, &mut check_site)?;
        match op {
            EnterTry { .. } => {
                let (_, a) = bounds(ops, at)?;
                if arm != 0 {
                    let (c, parent_after) = bounds(ops, opener(arm)?)?;
                    let closer = if arm & ACTIVE == 0 {
                        c - 1
                    } else {
                        parent_after - 1
                    };
                    if a > closer {
                        return Err(bad());
                    }
                }
                depth += 1;
                if usize::from(depth) > MAX_REGIONS {
                    return Err(bad());
                }
                arm = (at + 1) as u16;
            }
            LeaveTry { enter } => {
                let (c, _) = bounds(ops, usize::from(enter))?;
                if arm != enter + 1 || at != c - 1 {
                    return Err(bad());
                }
                arm |= ACTIVE; // lexical traversal, not a normal successor
            }
            EnterCatch { enter } => {
                let (c, _) = bounds(ops, usize::from(enter))?;
                if arm != (enter + 1) | ACTIVE || at != c {
                    return Err(bad());
                }
            }
            LeaveCatch { enter } => {
                let (_, a) = bounds(ops, usize::from(enter))?;
                if arm != (enter + 1) | ACTIVE || at != a - 1 {
                    return Err(bad());
                }
                let parent = shape[usize::from(enter)];
                arm = parent.arm;
                depth = parent.depth;
            }
            LoadCatch(slot) | StoreCatch { slot, .. } | UpdateCatch { slot, .. } => {
                let b = usize::from(slot.enter);
                let (c, a) = bounds(ops, b)?;
                if !(c + 1..a - 1).contains(&at) || shape[b].depth + 1 != slot.depth {
                    return Err(bad());
                }
            }
            ThrowValue { start, end } if start >= end || end > source_len => return Err(bad()),
            LoadLocal(slot) | StoreLocal { slot, .. } | UpdateLocal { slot, .. }
                if locals.is_none_or(|n| usize::from(slot) >= n) =>
            {
                return Err(bad());
            }
            Return { .. } if locals.is_none() => return Err(bad()),
            Call {
                start, end, argc, ..
            } if start >= end || end > source_len || argc > realm::MAX_PARAMETERS => {
                return Err(bad());
            }
            _ => {}
        }
    }
    if arm != 0 || packet != 0 || depth != 0 {
        return Err(bad());
    }
    shape.push(Shape {
        arm,
        packet,
        height: UNSEEDED,
        depth,
    });
    for (at, &op) in ops.iter().enumerate() {
        charge(1)?;
        if shape[at].height == UNSEEDED {
            // Canonical statement frontier of a disconnected island. An
            // expression alternate always has an earlier forward predecessor;
            // even a skipped classic-for update starts at statement height.
            shape[at].height = 0;
        }
        let row = shape[at];
        let h = usize::from(row.height);
        let next = at + 1;
        let p = usize::from(row.packet).checked_sub(1);
        let has = |want: usize| -> Result<(), E> { if h == want { Ok(()) } else { Err(bad()) } };
        let (anchor, floor) = if let Some(p) = p {
            let absolute = usize::from(shape[p].height)
                .checked_sub(1)
                .ok_or_else(bad)?;
            (
                absolute,
                absolute + usize::from(matches!(ops[p], PrepareDotCall(_))),
            )
        } else {
            (0, 0)
        };
        let effect = |pop, push| -> Result<usize, E> {
            Ok(h.checked_sub(pop)
                .filter(|&base| base >= floor)
                .ok_or_else(bad)?
                + push)
        };
        let packet_out = match op {
            FormDot(_) | PrepareDotCall(_) => next as u16,
            PutDot(_) => shape[p.ok_or_else(bad)?].packet,
            Call { member, .. } if member != BARE_CALL => shape[p.ok_or_else(bad)?].packet,
            _ => row.packet,
        };
        let mut edge = |shape: &mut [Shape],
                        target: usize,
                        arm: u16,
                        height: usize,
                        exceptional: bool|
         -> Result<(), E> {
            charge(1)?;
            let dest = shape.get_mut(target).ok_or_else(bad)?;
            if dest.arm != arm
                || dest.packet != packet_out
                || height > peak
                || matches!(ops.get(target), Some(EnterCatch { .. })) != exceptional
                || (dest.height != UNSEEDED && usize::from(dest.height) != height)
                || (target == ops.len() && height != exit.arity())
            {
                return Err(bad());
            }
            dest.height = height as u16;
            Ok(())
        };
        match op {
            EnterTry { catch, .. } => {
                has(0)?;
                edge(&mut shape, next, next as u16, 0, false)?;
                edge(
                    &mut shape,
                    usize::from(catch),
                    next as u16 | ACTIVE,
                    0,
                    true,
                )?;
            }
            LeaveTry { enter } | LeaveCatch { enter } => {
                has(0)?;
                let b = usize::from(enter);
                let (_, after) = bounds(ops, b)?;
                // Parent is at opener, whether this is the protected skip or
                // the caught body's fallthrough.
                let parent = shape[b].arm;
                edge(&mut shape, after, parent, 0, false)?;
            }
            EnterCatch { .. } => {
                has(0)?;
                edge(&mut shape, next, row.arm, 0, false)?;
            }
            ExitRegions {
                target,
                pop,
                backward,
            } => {
                has(0)?;
                let target = usize::from(target);
                let dest = *shape.get(target).ok_or_else(bad)?;
                if pop == 0
                    || row.depth.checked_sub(pop) != Some(dest.depth)
                    || (if backward { target >= at } else { target <= at })
                {
                    return Err(bad());
                }
                if dest.arm != 0 {
                    let b = opener(dest.arm)?;
                    let (c, a) = bounds(ops, b)?;
                    let interval = if dest.arm & ACTIVE == 0 {
                        b + 1..c
                    } else {
                        c..a
                    };
                    if !interval.contains(&at) {
                        return Err(bad());
                    }
                }
                edge(&mut shape, target, dest.arm, 0, false)?;
            }
            ThrowValue { .. } => has(1)?,
            Return { value } => has(usize::from(value))?,
            JumpIfFalse(target)
            | JumpIfFalseStatement(target)
            | ShortCircuitStatement { target, .. } => {
                if !matches!(op, JumpIfFalse(_)) {
                    has(1)?;
                }
                let h = effect(1, 0)?;
                edge(
                    &mut shape,
                    check_forward_target(next, target, ops.len())?,
                    row.arm,
                    h,
                    false,
                )?;
                edge(&mut shape, next, row.arm, h, false)?;
            }
            ShortCircuit { target, .. } => {
                edge(
                    &mut shape,
                    check_forward_target(next, target, ops.len())?,
                    row.arm,
                    effect(1, 1)?,
                    false,
                )?;
                edge(&mut shape, next, row.arm, effect(1, 0)?, false)?;
            }
            Jump(target) | JumpStatement(target) => {
                if matches!(op, JumpStatement(_)) {
                    has(0)?;
                }
                edge(
                    &mut shape,
                    check_forward_target(next, target, ops.len())?,
                    row.arm,
                    h,
                    false,
                )?;
            }
            LoopBack(target) => {
                has(0)?;
                if target >= at {
                    return Err(bad());
                }
                edge(&mut shape, target, row.arm, 0, false)?;
            }
            Call { argc, member, .. } => {
                let count = usize::from(argc) + 1;
                let h = if member == BARE_CALL {
                    effect(count, 1)?
                } else {
                    if h.checked_sub(count) != Some(anchor) {
                        return Err(bad());
                    }
                    anchor + 1
                };
                edge(&mut shape, next, row.arm, h, false)?;
            }
            PutDot(_) => {
                if h != anchor + 1 {
                    return Err(bad());
                }
                edge(&mut shape, next, row.arm, h, false)?;
            }
            _ => {
                let (pop, push) = match op {
                    Number(_)
                    | String(_)
                    | Boolean(_)
                    | Null
                    | Undefined
                    | CreateFunction(_)
                    | LoadBinding(_)
                    | LoadLocal(_)
                    | LoadCatch(_)
                    | UpdateBinding { .. }
                    | UpdateLocal { .. }
                    | UpdateCatch { .. } => (0, 1),
                    FormDot(_) | Discard => (1, 0),
                    StoreBinding(_)
                    | InitBinding { .. }
                    | StoreLocal { keep: false, .. }
                    | StoreCatch { keep: false, .. } => {
                        has(1)?;
                        (1, 0)
                    }
                    StoreBindingKeep(_)
                    | StoreLocal { keep: true, .. }
                    | StoreCatch { keep: true, .. }
                    | PrepareDotCall(_)
                    | GetDot(_)
                    | StringLength
                    | Positive
                    | Negate
                    | Not
                    | BitwiseNot
                    | Void
                    | Typeof => (1, 1),
                    TemplateAppend(_) | UpdateNumber(_) | Add | Subtract | Multiply | Divide
                    | Remainder | Exponentiate | BitwiseAnd | BitwiseXor | BitwiseOr
                    | ShiftLeft | ShiftRight | ShiftRightUnsigned | LooseEqual | LooseNotEqual
                    | StrictEqual | StrictNotEqual | LessThan | LessThanOrEqual | GreaterThan
                    | GreaterThanOrEqual => (2, 1),
                    _ => return Err(bad()),
                };
                edge(&mut shape, next, row.arm, effect(pop, push)?, false)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod limits {
    use super::*;
    use Instruction::*;

    const EMPTY: [Instruction; 4] = [
        EnterTry { catch: 2, after: 4 },
        LeaveTry { enter: 0 },
        EnterCatch { enter: 0 },
        LeaveCatch { enter: 0 },
    ];

    #[test]
    fn exact_records_and_combined_slot_reservation() {
        let mut frame = Frame::root(&EMPTY, 0, ExitStack::Empty);
        let mut rows = Regions::default();
        let mut fuel = 2 * MAX_REGIONS;
        for _ in 0..MAX_REGIONS {
            rows.enter(0, &frame, 0, 0, &mut fuel).unwrap();
        }
        assert_eq!(fuel, 0);
        assert_eq!(
            rows.enter(0, &frame, 0, 0, &mut fuel),
            Err(EvalError::LiveSlotLimitExceeded)
        );
        fuel = MAX_REGIONS - 1;
        assert_eq!(
            rows.drop_local(0, MAX_REGIONS, &mut fuel),
            Err(EvalError::DispatchLimitExceeded)
        );
        assert_eq!(rows.len(), MAX_REGIONS);
        fuel += 1;
        rows.drop_local(0, MAX_REGIONS, &mut fuel).unwrap();
        assert_eq!(fuel, 0);
        frame.temps = (MAX_LIVE_SLOTS - 4) as u16;
        frame.max_operands = 1;
        frame.fence = 1;
        fuel = 2;
        rows.enter(0, &frame, 0, 1, &mut fuel).unwrap();
        assert_eq!(
            rows.enter(0, &frame, 0, 1, &mut fuel),
            Err(EvalError::LiveSlotLimitExceeded)
        );
        assert_eq!(fuel, 1);
    }

    #[test]
    fn unwind_fuel_precedes_any_state_change_and_restores_raw_receiver() {
        let ops = [
            EnterTry { catch: 3, after: 5 },
            Undefined,
            LeaveTry { enter: 0 },
            EnterCatch { enter: 0 },
            LeaveCatch { enter: 0 },
        ];
        let mut rows = Regions::default();
        let mut catcher = Frame::root(&ops, 1, ExitStack::Empty);
        catcher.ip = 2;
        catcher.temps = 2;
        catcher.fence = 1;
        catcher.receiver = Some(Operand::Scalar(Value::Number(40.0)));
        let mut fuel = MAX_REGIONS + 1;
        rows.enter(0, &catcher, 1, 1, &mut fuel).unwrap();
        let mut current = Frame::root(&EMPTY, 0, ExitStack::Empty);
        current.region_fence = 1;
        current.receiver = Some(Operand::Scalar(Value::Number(41.0)));
        let mut callers = vec![Frame::root(&EMPTY, 0, ExitStack::Empty), catcher];
        let mut slots = vec![
            Operand::Scalar(Value::Number(10.0)),
            Operand::Scalar(Value::Number(20.0)),
            UNDEFINED,
        ];
        let mut packets = Packets {
            rows: vec![
                Packet {
                    captured: UNDEFINED,
                    site: 0,
                    anchor: 0,
                    kind: PacketKind::Form
                };
                2
            ],
            tier: 2,
        };
        let mut receivers = 2;
        let mut bindings = LegacyBindings::new(0).unwrap();
        let thrown = Thrown {
            value: Operand::Scalar(Value::Number(42.0)),
            owner: None,
            span: Span { start: 1, end: 2 },
        };
        macro_rules! transfer {
            () => {
                rows.transfer(
                    thrown,
                    Unwind {
                        frame: &mut current,
                        callers: &mut callers,
                        slots: &mut slots,
                        packets: &mut packets,
                        receivers: &mut receivers,
                        fuel: &mut fuel,
                    },
                    &mut bindings,
                )
            };
        }
        fuel = 3; // visit + frame traversal and removal + active install = 4
        assert_eq!(transfer!(), Err(EvalError::DispatchLimitExceeded));
        assert_eq!(
            (callers.len(), slots.len(), packets.rows.len(), receivers),
            (2, 3, 2, 2)
        );
        fuel = 4;
        assert_eq!(transfer!(), Ok(true));
        assert_eq!(
            (
                callers.len(),
                slots.len(),
                packets.rows.len(),
                receivers,
                current.ip,
                rows.len()
            ),
            (1, 2, 1, 1, 3, 1)
        );
        assert!(matches!(
            current.receiver,
            Some(Operand::Scalar(Value::Number(40.0)))
        ));
        assert!(matches!(
            rows.0[0].value,
            Operand::Scalar(Value::Number(42.0))
        ));
        assert_eq!(fuel, 0);
    }
}
