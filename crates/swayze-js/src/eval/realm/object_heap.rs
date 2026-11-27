//! Dormant Realm-private heap. IDs are logical slots, issued only by the owning
//! Realm, never exposed or mixed across Realms. There is no reuse, prototype
//! mutation, collection or reentry; dropping the owner frees even cyclic edges.
//!
//! Integration: keep one ObjectId per existing FunctionId, e.g. in the existing
//! function-origin row. Hoists and reached expressions must stage that row, code
//! retention and the eager object/prototype batch before any logical publication.
//! For multiple hoists, feed one bounded batch through `stage` (using bounded
//! caller-owned staging in place of fixed arrays), including roots if absent;
//! do not publish one `stage_function` at a time during GDI preflight.
//! Infallible Realm::new need not allocate: stage the roots at a fallible seam.
//!
//! No key/value byte store here. `Query` borrows already resolved UTF-16; future
//! callers must resolve against the actual operand/key origin, never the current
//! frame by default. `SetPlan` identifies whether a key is needed. Split the
//! existing `Arena::append` into shared plan/reserve/publish so a new key and
//! transient value can share the existing copy/retention/peak limits and publish
//! with the descriptor. Give that plan checked ranges for its own future writes;
//! today's prepare path validates only already published ranges. Reuse static,
//! empty and owned values; do not retain on reads, rejection or replacement of
//! an existing key. No string copies or shared-transaction atomicity are tested
//! by this module. Fold its work/request meters into Realm/GDI limits at wiring.

use std::mem::size_of;

use super::{FunctionId, MAX_FUNCTIONS, RealmRange, StoredValue, Value};

const OBJECTS: usize = 1280;
const ROWS: usize = 4096;
const OWN_ROWS: usize = 64;
const KEY_UNITS: usize = 256;
const WALK_HOPS: usize = 32;
const WORK: usize = 131_072;
const REQUEST_BYTES: usize = 512 * 1024;
const ROOT_OBJECTS: usize = 4;
const ROOT_ROWS: usize = 27;
const FUNCTION_OBJECTS: usize = 2;
const FUNCTION_ROWS: usize = 4;
const _: () = assert!(OBJECTS <= u16::MAX as usize);
const _: () = assert!(ROWS <= u16::MAX as usize);
const _: () = assert!(OWN_ROWS <= u8::MAX as usize);
const _: () = assert!(ROOT_OBJECTS + MAX_FUNCTIONS * FUNCTION_OBJECTS <= OBJECTS);
const _: () = assert!(ROOT_ROWS + MAX_FUNCTIONS * FUNCTION_ROWS <= ROWS);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObjectId(u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RowId(u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Ordinary,
    Function(FunctionId),
    FunctionPrototype, // callable, not constructible
    ObjectConstructor, // callable and constructible, bodies not supplied
    FunctionConstructor,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum FieldValue {
    Stored(StoredValue),
    Object(ObjectId), // including callables; no second property identity
    StaticString(&'static [u16]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Attrs {
    writable: bool,
    enumerable: bool,
    configurable: bool,
}

const METHOD: Attrs = Attrs {
    writable: true,
    enumerable: false,
    configurable: true,
};
const META: Attrs = Attrs {
    writable: false,
    enumerable: false,
    configurable: true,
};
const FUNCTION_PROTO: Attrs = Attrs {
    writable: true,
    enumerable: false,
    configurable: false,
};
const INTRINSIC_PROTO: Attrs = Attrs {
    writable: false,
    enumerable: false,
    configurable: false,
};
const DEFAULT: Attrs = Attrs {
    writable: true,
    enumerable: true,
    configurable: true,
};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Descriptor {
    Data(FieldValue, Attrs),
    // The data descriptor's attributes are known, its builtin identity isn't
    // installed. Get stops, but Set can use ordinary data-descriptor rules.
    BuiltinData(Attrs),
    RestrictedAccessor,
    AnnexB, // optional methods/accessor: neither presence nor behavior selected
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Barrier {
    BuiltinValue,
    Accessor { receiver: ObjectId },
    AnnexB,
    Incomplete(ObjectId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    ObjectId,
    RowId,
    Key,
    Value,
    Order,
    FunctionLimit,
    ObjectLimit,
    RowLimit,
    OwnRowLimit,
    KeyLimit,
    HopLimit,
    WorkLimit,
    RequestLimit,
    Allocation,
    Unsupported(Barrier),
}

#[derive(Debug)]
struct Budget {
    work: usize,
    requests: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            work: WORK,
            requests: REQUEST_BYTES,
        }
    }
}

impl Budget {
    fn work(&mut self, amount: usize) -> Result<(), Fault> {
        self.work = self.work.checked_sub(amount).ok_or(Fault::WorkLimit)?;
        Ok(())
    }

    fn request(&mut self, request: Request) -> Result<(), Fault> {
        self.requests = self
            .requests
            .checked_sub(request.bytes)
            .ok_or(Fault::RequestLimit)?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Query<'a> {
    key: &'a [u16],
    retained: &'a [u16],
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Key {
    Ascii(&'static [u8]), // trusted immutable intrinsic spelling
    Retained(RealmRange),
    Empty,
}

impl Key {
    fn matches(self, query: Query<'_>, budget: &mut Budget) -> Result<bool, Fault> {
        fn exact(
            stored: impl ExactSizeIterator<Item = u16>,
            query: &[u16],
            budget: &mut Budget,
        ) -> Result<bool, Fault> {
            if stored.len() != query.len() {
                return Ok(false);
            }
            for (a, &b) in stored.zip(query) {
                budget.work(1)?;
                if a != b {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        match self {
            Self::Ascii(bytes) => exact(bytes.iter().copied().map(u16::from), query.key, budget),
            Self::Retained(range) => {
                let units = range
                    .get(query.retained)
                    .filter(|s| s.len() <= KEY_UNITS)
                    .ok_or(Fault::Key)?;
                exact(units.iter().copied(), query.key, budget)
            }
            Self::Empty => Ok(query.key.is_empty()),
        }
    }
}

impl FieldValue {
    fn validate(self, object_end: usize, retained: &[u16]) -> Result<(), Fault> {
        match self {
            Self::Object(ObjectId(id)) if usize::from(id) >= object_end => Err(Fault::ObjectId),
            // Legacy function operands must be canonicalized at the owning Realm seam.
            Self::Stored(StoredValue::Function(_)) => Err(Fault::Value),
            Self::Stored(StoredValue::Realm(range)) if range.get(retained).is_none() => {
                Err(Fault::Value)
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Row {
    key: Key,
    desc: Descriptor,
    next: Option<RowId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Object {
    kind: Kind,
    prototype: Option<ObjectId>,
    head: Option<RowId>,
    count: u8,
    complete: bool, // only the ordinary, string-key domain of this module
}

impl Object {
    const fn new(kind: Kind, prototype: Option<ObjectId>, complete: bool) -> Self {
        Self {
            kind,
            prototype,
            head: None,
            count: 0,
            complete,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Roots {
    object_proto: ObjectId,
    function_proto: ObjectId,
    object_ctor: ObjectId,
    function_ctor: ObjectId,
}

#[derive(Default)]
struct Heap {
    objects: Vec<Object>,
    rows: Vec<Row>,
    object_tier: usize,
    row_tier: usize,
    roots: Option<Roots>,
    functions: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Request {
    bytes: usize, // new buffer request; not allocator capacity or usable size
    moved: usize, // old initialized slots copied if this plan publishes
}

type ReserveHook<'a> = &'a mut dyn FnMut(Request) -> Result<(), Fault>;

#[derive(Default)]
struct Growth {
    objects: Option<(usize, Vec<Object>)>,
    rows: Option<(usize, Vec<Row>)>,
}

fn reserve<T>(
    old_len: usize,
    old_tier: usize,
    end: usize,
    limit: usize,
    budget: &mut Budget,
    hook: ReserveHook<'_>,
) -> Result<Option<(usize, Vec<T>)>, Fault> {
    if end <= old_tier {
        return Ok(None);
    }
    // Always stage a fresh buffer on a logical tier transition, even if an
    // allocator happened to overallocate the old request. No old state is
    // touched and there are no relocations until all reservations succeed.
    let tier = end.next_power_of_two().min(limit);
    let request = Request {
        bytes: tier * size_of::<T>(),
        moved: old_len,
    };
    budget.work(request.moved)?;
    budget.request(request)?;
    hook(request)?;
    let mut backing = Vec::new();
    backing
        .try_reserve_exact(tier)
        .map_err(|_| Fault::Allocation)?;
    Ok(Some((tier, backing)))
}

impl Growth {
    fn install(self, heap: &mut Heap) {
        if let Some((tier, mut backing)) = self.objects {
            backing.extend_from_slice(&heap.objects);
            heap.objects = backing;
            heap.object_tier = tier;
        }
        if let Some((tier, mut backing)) = self.rows {
            backing.extend_from_slice(&heap.rows);
            heap.rows = backing;
            heap.row_tier = tier;
        }
    }
}

// One request for each new backing. Worst old+new overlap (even if both
// grow together): 2304 Object + 6144 Row slots, plus the fixed publication
// plan (at most the bootstrap plan below). No claim about allocator rounding,
// Rust stack temporaries, or the future shared String/artifact arenas' peak.
const BACKING_PEAK: usize =
    (1024 + OBJECTS) * size_of::<Object>() + (2048 + ROWS) * size_of::<Row>();

struct Append<'a, const N: usize, const R: usize> {
    heap: &'a mut Heap,
    growth: Growth,
    nodes: [Object; N],
    rows: [(ObjectId, Key, Descriptor); R],
    roots: Option<Roots>,
    add_function: bool,
}

impl<const N: usize, const R: usize> Append<'_, N, R> {
    fn publish(self) -> ObjectId {
        let first = ObjectId(self.heap.objects.len() as u16);
        self.growth.install(self.heap);
        self.heap.objects.extend_from_slice(&self.nodes);
        for (owner, key, desc) in self.rows {
            self.heap.push_row(owner, key, desc);
        }
        if let Some(roots) = self.roots {
            self.heap.roots = Some(roots);
        }
        self.heap.functions += usize::from(self.add_function);
        first
    }
}

// Affine plans hold the only mutable loan of the heap until dropped or
// published. No stale or cross-heap row/append plan can be replayed.
enum Edit {
    Replace(RowId, Attrs),
    Insert(ObjectId),
}

struct SetPlan<'a, 'key> {
    heap: &'a mut Heap,
    edit: Edit,
    query: Query<'key>,
}

struct Write<'a> {
    heap: &'a mut Heap,
    edit: Edit,
    desc: Descriptor,
    key: Option<Key>,
    growth: Growth,
}

impl SetPlan<'_, '_> {
    fn needs_key(&self) -> bool {
        matches!(self.edit, Edit::Insert(_))
    }
}

impl<'a> SetPlan<'a, '_> {
    fn prepare(
        self,
        key: Option<Key>,
        value: FieldValue,
        budget: &mut Budget,
        hook: ReserveHook<'_>,
    ) -> Result<Write<'a>, Fault> {
        value.validate(self.heap.objects.len(), self.query.retained)?;
        let (attrs, extra) = match (&self.edit, key) {
            (Edit::Replace(_, attrs), None) => (*attrs, 0),
            (Edit::Insert(_), Some(key)) if key.matches(self.query, budget)? => (DEFAULT, 1),
            _ => return Err(Fault::Key),
        };
        budget.work(1)?; // one descriptor replacement or row/head publication
        let growth = self.heap.grow(
            self.heap.objects.len(),
            self.heap.rows.len() + extra,
            budget,
            hook,
        )?;
        Ok(Write {
            heap: self.heap,
            edit: self.edit,
            desc: Descriptor::Data(value, attrs),
            key,
            growth,
        })
    }
}

impl Write<'_> {
    fn publish(self) {
        self.growth.install(self.heap);
        match self.edit {
            Edit::Replace(row, _) => self.heap.rows[usize::from(row.0)].desc = self.desc,
            Edit::Insert(owner) => {
                self.heap
                    .push_row(owner, self.key.expect("checked insert key"), self.desc)
            }
        }
    }
}

impl Heap {
    fn grow(
        &self,
        objects: usize,
        rows: usize,
        budget: &mut Budget,
        hook: ReserveHook<'_>,
    ) -> Result<Growth, Fault> {
        if objects > OBJECTS {
            return Err(Fault::ObjectLimit);
        }
        if rows > ROWS {
            return Err(Fault::RowLimit);
        }
        Ok(Growth {
            objects: reserve(
                self.objects.len(),
                self.object_tier,
                objects,
                OBJECTS,
                budget,
                hook,
            )?,
            rows: reserve(self.rows.len(), self.row_tier, rows, ROWS, budget, hook)?,
        })
    }

    fn object(&self, id: ObjectId) -> Result<&Object, Fault> {
        self.objects.get(usize::from(id.0)).ok_or(Fault::ObjectId)
    }

    fn push_row(&mut self, owner: ObjectId, key: Key, desc: Descriptor) {
        let object = &mut self.objects[usize::from(owner.0)];
        let slot = RowId(self.rows.len() as u16);
        self.rows.push(Row {
            key,
            desc,
            next: object.head,
        });
        object.head = Some(slot);
        object.count += 1;
    }

    fn own(
        &self,
        object: &Object,
        query: Query<'_>,
        budget: &mut Budget,
    ) -> Result<Option<(RowId, Descriptor)>, Fault> {
        let mut next = object.head;
        for _ in 0..object.count {
            budget.work(1)?;
            let id = next.ok_or(Fault::RowId)?;
            let row = self.rows.get(usize::from(id.0)).ok_or(Fault::RowId)?;
            if row.key.matches(query, budget)? {
                return Ok(Some((id, row.desc)));
            }
            next = row.next;
        }
        if next.is_some() {
            return Err(Fault::RowId);
        }
        Ok(None)
    }

    fn lookup(
        &self,
        target: ObjectId,
        query: Query<'_>,
        budget: &mut Budget,
    ) -> Result<Option<(ObjectId, RowId, Descriptor)>, Fault> {
        if query.key.len() > KEY_UNITS {
            return Err(Fault::KeyLimit);
        }
        let mut id = target;
        for _ in 0..WALK_HOPS {
            budget.work(1)?;
            let object = self.object(id)?;
            if let Some((row, desc)) = self.own(object, query, budget)? {
                return Ok(Some((id, row, desc)));
            }
            if !object.complete {
                return Err(Fault::Unsupported(Barrier::Incomplete(id)));
            }
            let Some(parent) = object.prototype else {
                return Ok(None);
            };
            id = parent;
        }
        Err(Fault::HopLimit)
    }

    fn get(
        &self,
        target: ObjectId,
        receiver: ObjectId,
        query: Query<'_>,
        budget: &mut Budget,
    ) -> Result<Option<FieldValue>, Fault> {
        self.object(receiver)?;
        match self.lookup(target, query, budget)? {
            None => Ok(None),
            Some((_, _, Descriptor::Data(value, _))) => {
                value.validate(self.objects.len(), query.retained)?;
                Ok(Some(value))
            }
            Some((_, _, Descriptor::BuiltinData(_))) => {
                Err(Fault::Unsupported(Barrier::BuiltinValue))
            }
            Some((_, _, Descriptor::RestrictedAccessor)) => {
                Err(Fault::Unsupported(Barrier::Accessor { receiver }))
            }
            Some((_, _, Descriptor::AnnexB)) => Err(Fault::Unsupported(Barrier::AnnexB)),
        }
    }

    // None means OrdinarySet returned false. The caller will own strictness,
    // assignment results and error transport. Re-probe at the actual Set, not
    // before the RHS. All receivers admitted here are ordinary heap objects.
    fn set<'a, 'key>(
        &'a mut self,
        target: ObjectId,
        receiver: ObjectId,
        query: Query<'key>,
        budget: &mut Budget,
    ) -> Result<Option<SetPlan<'a, 'key>>, Fault> {
        let object = self.object(receiver)?;
        let edit = match self.lookup(target, query, budget)? {
            Some((_, _, Descriptor::RestrictedAccessor)) => {
                return Err(Fault::Unsupported(Barrier::Accessor { receiver }));
            }
            Some((_, _, Descriptor::AnnexB)) => return Err(Fault::Unsupported(Barrier::AnnexB)),
            Some((_, _, Descriptor::Data(_, attrs) | Descriptor::BuiltinData(attrs)))
                if !attrs.writable =>
            {
                return Ok(None);
            }
            Some((owner, row, Descriptor::Data(_, attrs) | Descriptor::BuiltinData(attrs)))
                if owner == receiver =>
            {
                Edit::Replace(row, attrs)
            }
            _ => {
                // When receiver == target, lookup already proved no own row.
                let own = if receiver == target {
                    None
                } else {
                    budget.work(1)?; // extra receiver own-descriptor probe
                    self.own(object, query, budget)?
                };
                match own {
                    Some((row, Descriptor::Data(_, attrs) | Descriptor::BuiltinData(attrs)))
                        if attrs.writable =>
                    {
                        Edit::Replace(row, attrs)
                    }
                    Some((_, Descriptor::AnnexB)) => {
                        return Err(Fault::Unsupported(Barrier::AnnexB));
                    }
                    Some(_) => return Ok(None), // receiver-only accessor is not invoked
                    None if !object.complete => {
                        return Err(Fault::Unsupported(Barrier::Incomplete(receiver)));
                    }
                    None if usize::from(object.count) == OWN_ROWS => {
                        return Err(Fault::OwnRowLimit);
                    }
                    None => Edit::Insert(receiver),
                }
            }
        };
        Ok(Some(SetPlan {
            heap: self,
            edit,
            query,
        }))
    }

    fn stage<'a, const N: usize, const R: usize>(
        &'a mut self,
        nodes: [Object; N],
        rows: [(ObjectId, Key, Descriptor); R],
        retained: &[u16],
        budget: &mut Budget,
        hook: ReserveHook<'_>,
    ) -> Result<Append<'a, N, R>, Fault> {
        let base = self.objects.len();
        let end = base + N;
        // The only batch producers have fixed sizes. Each edge inspects a
        // logical slot, and staged prototypes must point strictly backwards.
        budget.work(2 * (N + R))?; // one validation and one publication per entry
        let mut counts = [0; N];
        for (i, node) in nodes.iter().enumerate() {
            if node
                .prototype
                .is_some_and(|id| usize::from(id.0) >= base + i)
            {
                return Err(Fault::ObjectId);
            }
        }
        for &(owner, _, desc) in &rows {
            let i = usize::from(owner.0)
                .checked_sub(base)
                .ok_or(Fault::ObjectId)?;
            let count = counts.get_mut(i).ok_or(Fault::ObjectId)?;
            *count += 1;
            if *count > OWN_ROWS {
                return Err(Fault::OwnRowLimit);
            }
            if let Descriptor::Data(value, _) = desc {
                value.validate(end, retained)?;
            }
        }
        let growth = self.grow(end, self.rows.len() + R, budget, hook)?;
        Ok(Append {
            heap: self,
            growth,
            nodes,
            rows,
            roots: None,
            add_function: false,
        })
    }

    fn stage_roots(
        &mut self,
        budget: &mut Budget,
        hook: ReserveHook<'_>,
    ) -> Result<Append<'_, ROOT_OBJECTS, ROOT_ROWS>, Fault> {
        if self.roots.is_some() || !self.objects.is_empty() {
            return Err(Fault::Order);
        }
        let [op, fp, oc, fc] = [0, 1, 2, 3].map(ObjectId);
        let nodes = [
            Object::new(Kind::Ordinary, None, true),
            Object::new(Kind::FunctionPrototype, Some(op), true),
            Object::new(Kind::ObjectConstructor, Some(fp), false),
            Object::new(Kind::FunctionConstructor, Some(fp), false),
        ];
        let object = FieldValue::StaticString(&[79, 98, 106, 101, 99, 116]);
        let function = FieldValue::StaticString(&[70, 117, 110, 99, 116, 105, 111, 110]);
        use Descriptor::{AnnexB, BuiltinData, Data, RestrictedAccessor};
        let rows = [
            (
                op,
                Key::Ascii(b"constructor"),
                Data(FieldValue::Object(oc), METHOD),
            ),
            (op, Key::Ascii(b"hasOwnProperty"), BuiltinData(METHOD)),
            (op, Key::Ascii(b"isPrototypeOf"), BuiltinData(METHOD)),
            (op, Key::Ascii(b"propertyIsEnumerable"), BuiltinData(METHOD)),
            (op, Key::Ascii(b"toLocaleString"), BuiltinData(METHOD)),
            (op, Key::Ascii(b"toString"), BuiltinData(METHOD)),
            (op, Key::Ascii(b"valueOf"), BuiltinData(METHOD)),
            (op, Key::Ascii(b"__defineGetter__"), AnnexB),
            (op, Key::Ascii(b"__defineSetter__"), AnnexB),
            (op, Key::Ascii(b"__lookupGetter__"), AnnexB),
            (op, Key::Ascii(b"__lookupSetter__"), AnnexB),
            (op, Key::Ascii(b"__proto__"), AnnexB),
            (
                fp,
                Key::Ascii(b"constructor"),
                Data(FieldValue::Object(fc), METHOD),
            ),
            (
                fp,
                Key::Ascii(b"name"),
                Data(FieldValue::Stored(StoredValue::Empty), META),
            ),
            (fp, Key::Ascii(b"length"), Data(number(0), META)),
            (fp, Key::Ascii(b"apply"), BuiltinData(METHOD)),
            (fp, Key::Ascii(b"bind"), BuiltinData(METHOD)),
            (fp, Key::Ascii(b"call"), BuiltinData(METHOD)),
            (fp, Key::Ascii(b"toString"), BuiltinData(METHOD)),
            (fp, Key::Ascii(b"arguments"), RestrictedAccessor),
            (fp, Key::Ascii(b"caller"), RestrictedAccessor),
            (
                oc,
                Key::Ascii(b"prototype"),
                Data(FieldValue::Object(op), INTRINSIC_PROTO),
            ),
            (oc, Key::Ascii(b"name"), Data(object, META)),
            (oc, Key::Ascii(b"length"), Data(number(1), META)),
            (
                fc,
                Key::Ascii(b"prototype"),
                Data(FieldValue::Object(fp), INTRINSIC_PROTO),
            ),
            (fc, Key::Ascii(b"name"), Data(function, META)),
            (fc, Key::Ascii(b"length"), Data(number(1), META)),
        ];
        let mut plan = self.stage(nodes, rows, &[], budget, hook)?;
        plan.roots = Some(Roots {
            object_proto: op,
            function_proto: fp,
            object_ctor: oc,
            function_ctor: fc,
        });
        Ok(plan)
    }

    fn stage_function<'a>(
        &'a mut self,
        id: FunctionId,
        name: FieldValue,
        formals: u8,
        retained: &[u16],
        budget: &mut Budget,
        hook: ReserveHook<'_>,
    ) -> Result<Append<'a, FUNCTION_OBJECTS, FUNCTION_ROWS>, Fault> {
        let roots = self.roots.ok_or(Fault::Order)?;
        if self.functions >= MAX_FUNCTIONS {
            return Err(Fault::FunctionLimit);
        }
        if usize::from(id.0) != self.functions {
            return Err(Fault::Order);
        }
        if formals > super::MAX_PARAMETERS
            || !matches!(
                name,
                FieldValue::Stored(StoredValue::Empty | StoredValue::Realm(_))
                    | FieldValue::StaticString(_)
            )
        {
            return Err(Fault::Value);
        }
        let f = ObjectId(self.objects.len() as u16);
        let p = ObjectId(f.0 + 1);
        let nodes = [
            Object::new(Kind::Function(id), Some(roots.function_proto), true),
            Object::new(Kind::Ordinary, Some(roots.object_proto), true),
        ];
        let rows = [
            (f, Key::Ascii(b"name"), Descriptor::Data(name, META)),
            (
                f,
                Key::Ascii(b"length"),
                Descriptor::Data(number(formals), META),
            ),
            (
                f,
                Key::Ascii(b"prototype"),
                Descriptor::Data(FieldValue::Object(p), FUNCTION_PROTO),
            ),
            (
                p,
                Key::Ascii(b"constructor"),
                Descriptor::Data(FieldValue::Object(f), METHOD),
            ),
        ];
        let mut plan = self.stage(nodes, rows, retained, budget, hook)?;
        plan.add_function = true;
        Ok(plan)
    }

    fn stage_ordinary(
        &mut self,
        proto: Option<ObjectId>,
        budget: &mut Budget,
        hook: ReserveHook<'_>,
    ) -> Result<Append<'_, 1, 0>, Fault> {
        if self.roots.is_none() {
            return Err(Fault::Order);
        }
        self.stage(
            [Object::new(Kind::Ordinary, proto, true)],
            [],
            &[],
            budget,
            hook,
        )
    }
}

fn number(n: u8) -> FieldValue {
    FieldValue::Stored(StoredValue::Scalar(Value::Number(f64::from(n))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;
    use crate::eval::realm::Arena;

    const EMPTY: FieldValue = FieldValue::Stored(StoredValue::Empty);

    fn ok(_: Request) -> Result<(), Fault> {
        Ok(())
    }

    fn rooted() -> (Heap, Roots) {
        let mut heap = Heap::default();
        assert!(heap.objects.is_empty() && heap.rows.is_empty());
        heap.stage_roots(&mut Budget::default(), &mut ok)
            .unwrap()
            .publish();
        let roots = heap.roots.unwrap();
        (heap, roots)
    }

    fn ordinary(heap: &mut Heap, proto: Option<ObjectId>) -> ObjectId {
        heap.stage_ordinary(proto, &mut Budget::default(), &mut ok)
            .unwrap()
            .publish()
    }

    fn query(key: &[u16]) -> Query<'_> {
        Query { key, retained: &[] }
    }

    fn units(ascii: &[u8]) -> Vec<u16> {
        ascii.iter().copied().map(u16::from).collect()
    }

    fn read(heap: &Heap, target: ObjectId, name: &[u8]) -> Result<Option<FieldValue>, Fault> {
        heap.get(target, target, query(&units(name)), &mut Budget::default())
    }

    fn put(
        heap: &mut Heap,
        target: ObjectId,
        name: &'static [u8],
        value: FieldValue,
    ) -> Result<bool, Fault> {
        let units = units(name);
        let mut budget = Budget::default();
        let Some(plan) = heap.set(target, target, query(&units), &mut budget)? else {
            return Ok(false);
        };
        let key = plan.needs_key().then_some(Key::Ascii(name));
        plan.prepare(key, value, &mut budget, &mut ok)?.publish();
        Ok(true)
    }

    fn retain(arena: &mut Arena, units: &[u16]) -> RealmRange {
        let Ok(StoredValue::Realm(range)) = arena.append(
            units,
            &mut 0,
            Span { start: 0, end: 0 },
            Vec::try_reserve_exact,
        ) else {
            panic!("test input must be an already published Realm range");
        };
        range
    }

    #[test]
    fn roots_have_real_links_and_explicit_missing_behavior() {
        let (mut heap, roots) = rooted();
        let Roots {
            object_proto: op,
            function_proto: fp,
            object_ctor: oc,
            function_ctor: fc,
        } = roots;
        assert_eq!(
            (heap.objects.len(), heap.rows.len()),
            (ROOT_OBJECTS, ROOT_ROWS)
        );
        assert_eq!(heap.object(op).unwrap().prototype, None);
        assert_eq!(heap.object(fp).unwrap().prototype, Some(op));
        assert_eq!(heap.object(fp).unwrap().kind, Kind::FunctionPrototype);
        for (proto, ctor, kind) in [
            (op, oc, Kind::ObjectConstructor),
            (fp, fc, Kind::FunctionConstructor),
        ] {
            assert_eq!(heap.object(ctor).unwrap().kind, kind);
            assert_eq!(heap.object(ctor).unwrap().prototype, Some(fp));
            assert_eq!(
                read(&heap, proto, b"constructor"),
                Ok(Some(FieldValue::Object(ctor)))
            );
            assert_eq!(
                read(&heap, ctor, b"prototype"),
                Ok(Some(FieldValue::Object(proto)))
            );
            assert_eq!(put(&mut heap, ctor, b"prototype", EMPTY), Ok(false));
            assert_eq!(read(&heap, ctor, b"length"), Ok(Some(number(1))));
        }
        assert_eq!(read(&heap, op, b"absent"), Ok(None));
        assert_eq!(read(&heap, fp, b"prototype"), Ok(None));
        assert_eq!(
            read(&heap, fc, b"unreviewed"),
            Err(Fault::Unsupported(Barrier::Incomplete(fc)))
        );
        assert_eq!(
            put(&mut heap, oc, b"unreviewed", EMPTY),
            Err(Fault::Unsupported(Barrier::Incomplete(oc)))
        );
        assert_eq!(
            read(&heap, op, b"toString"),
            Err(Fault::Unsupported(Barrier::BuiltinValue))
        );
        assert_eq!(
            read(&heap, op, b"__proto__"),
            Err(Fault::Unsupported(Barrier::AnnexB))
        );
        assert_eq!(
            heap.stage_roots(&mut Budget::default(), &mut ok).err(),
            Some(Fault::Order)
        );
    }

    #[test]
    fn eager_functions_are_fresh_and_have_initial_attributes() {
        let (mut heap, roots) = rooted();
        let mut arena = Arena::default();
        let name = FieldValue::Stored(StoredValue::Realm(retain(&mut arena, &[0x41, 0xd800])));
        let f = heap
            .stage_function(
                FunctionId(0),
                name,
                2,
                arena.units(),
                &mut Budget::default(),
                &mut ok,
            )
            .unwrap()
            .publish();
        let g = heap
            .stage_function(
                FunctionId(1),
                EMPTY,
                0,
                &[],
                &mut Budget::default(),
                &mut ok,
            )
            .unwrap()
            .publish();
        let proto = |id| match read(&heap, id, b"prototype") {
            Ok(Some(FieldValue::Object(p))) => p,
            _ => panic!("missing eager prototype"),
        };
        let p = proto(f);
        let q = proto(g);
        assert_ne!(p, q);
        assert_eq!(heap.object(f).unwrap().kind, Kind::Function(FunctionId(0)));
        assert_eq!(
            heap.object(f).unwrap().prototype,
            Some(roots.function_proto)
        );
        assert_eq!(heap.object(p).unwrap().prototype, Some(roots.object_proto));
        assert_eq!(
            read(&heap, p, b"constructor"),
            Ok(Some(FieldValue::Object(f)))
        );
        assert_eq!(
            read(&heap, q, b"constructor"),
            Ok(Some(FieldValue::Object(g)))
        );
        assert_eq!(
            heap.get(
                f,
                f,
                Query {
                    key: &units(b"name"),
                    retained: arena.units()
                },
                &mut Budget::default()
            ),
            Ok(Some(name))
        );
        assert_eq!(read(&heap, g, b"name"), Ok(Some(EMPTY)));
        for (owner, key, attrs) in [
            (f, b"name".as_slice(), META),
            (f, b"length", META),
            (f, b"prototype", FUNCTION_PROTO),
            (p, b"constructor", METHOD),
        ] {
            let (_, _, Descriptor::Data(_, actual)) = heap
                .lookup(owner, query(&units(key)), &mut Budget::default())
                .unwrap()
                .unwrap()
            else {
                panic!()
            };
            assert_eq!(actual, attrs);
        }
        assert_eq!(read(&heap, f, b"length"), Ok(Some(number(2))));
        assert_eq!(put(&mut heap, f, b"length", number(7)), Ok(false));
        let old_instance = ordinary(&mut heap, Some(p));
        assert_eq!(
            put(&mut heap, f, b"prototype", FieldValue::Object(q)),
            Ok(true)
        );
        assert_eq!(heap.object(old_instance).unwrap().prototype, Some(p));
        assert_eq!(
            read(&heap, p, b"constructor"),
            Ok(Some(FieldValue::Object(f)))
        );
    }

    #[test]
    fn descriptor_set_uses_current_rows_and_original_receiver() {
        let (mut heap, roots) = rooted();
        let (op, fp) = (roots.object_proto, roots.function_proto);
        let receiver = ordinary(&mut heap, Some(op));
        assert_eq!(
            read(&heap, receiver, b"toString"),
            Err(Fault::Unsupported(Barrier::BuiltinValue))
        );
        let key = units(b"toString");
        let mut budget = Budget::default();
        {
            let plan = heap
                .set(op, receiver, query(&key), &mut budget)
                .unwrap()
                .unwrap();
            assert!(plan.needs_key());
            // Abandon this plan; no row edit can outlive its exclusive loan.
        }
        assert_eq!(put(&mut heap, receiver, b"toString", number(1)), Ok(true));
        let row_count = heap.rows.len();
        let plan = heap
            .set(op, receiver, query(&key), &mut budget)
            .unwrap()
            .unwrap();
        assert!(!plan.needs_key());
        plan.prepare(None, number(2), &mut budget, &mut ok)
            .unwrap()
            .publish();
        assert_eq!(heap.rows.len(), row_count);
        assert_eq!(read(&heap, receiver, b"toString"), Ok(Some(number(2))));
        assert_eq!(
            read(&heap, op, b"toString"),
            Err(Fault::Unsupported(Barrier::BuiltinValue))
        );
        let (_, _, desc) = heap
            .lookup(receiver, query(&key), &mut budget)
            .unwrap()
            .unwrap();
        assert_eq!(desc, Descriptor::Data(number(2), DEFAULT));
        let caller = units(b"caller");
        assert_eq!(
            heap.get(fp, receiver, query(&caller), &mut budget),
            Err(Fault::Unsupported(Barrier::Accessor { receiver }))
        );
        assert_eq!(
            heap.set(fp, receiver, query(&caller), &mut budget).err(),
            Some(Fault::Unsupported(Barrier::Accessor { receiver }))
        );
        // A missing target data descriptor still checks the receiver's own
        // descriptor; this receiver-only accessor is not invoked.
        assert!(
            heap.set(op, fp, query(&caller), &mut budget)
                .unwrap()
                .is_none()
        );
        assert!(
            heap.set(fp, receiver, query(&units(b"length")), &mut budget)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn exact_utf16_key_and_value_ranges_do_not_infer_origin() {
        let (mut heap, _) = rooted();
        let object = ordinary(&mut heap, None);
        let mut arena = Arena::default();
        let raw = [0, 0xd800, 0x0078];
        let range = retain(&mut arena, &raw);
        let different = retain(&mut arena, &[0, 0xfffd, 0x0078]);
        let committed = arena.used;
        let mut budget = Budget::default();
        let copy_from_elsewhere = Vec::from(raw);
        let q = Query {
            key: &copy_from_elsewhere,
            retained: arena.units(),
        };
        assert_eq!(
            heap.set(object, object, q, &mut budget)
                .unwrap()
                .unwrap()
                .prepare(Some(Key::Retained(different)), EMPTY, &mut budget, &mut ok)
                .err(),
            Some(Fault::Key)
        );
        let value = FieldValue::Stored(StoredValue::Realm(different));
        heap.set(object, object, q, &mut budget)
            .unwrap()
            .unwrap()
            .prepare(Some(Key::Retained(range)), value, &mut budget, &mut ok)
            .unwrap()
            .publish();
        assert_eq!(heap.get(object, object, q, &mut budget), Ok(Some(value)));
        assert_eq!(
            heap.get(
                object,
                object,
                Query {
                    key: &[0, 0xfffd, 0x0078],
                    ..q
                },
                &mut budget
            ),
            Ok(None)
        );
        assert_eq!(
            heap.get(object, object, Query { retained: &[], ..q }, &mut budget),
            Err(Fault::Key)
        );
        heap.set(object, object, q, &mut budget)
            .unwrap()
            .unwrap()
            .prepare(None, EMPTY, &mut budget, &mut ok)
            .unwrap()
            .publish();
        assert_eq!(arena.used, committed);
        heap.set(object, object, Query { key: &[], ..q }, &mut budget)
            .unwrap()
            .unwrap()
            .prepare(Some(Key::Empty), EMPTY, &mut budget, &mut ok)
            .unwrap()
            .publish();
        assert_eq!(
            heap.get(object, object, Query { key: &[], ..q }, &mut budget),
            Ok(Some(EMPTY))
        );
        assert_eq!(heap.rows.len(), ROOT_ROWS + 2);
    }

    #[test]
    fn dual_reserve_failure_and_dropped_publications_have_no_logical_effect() {
        let (mut heap, _) = rooted();
        let owner = ordinary(&mut heap, None);
        for _ in 0..3 {
            ordinary(&mut heap, None);
        }
        for key in [b"a", b"b", b"c", b"d", b"e"] {
            put(&mut heap, owner, key, EMPTY).unwrap();
        }
        assert_eq!(
            (
                heap.objects.len(),
                heap.rows.len(),
                heap.object_tier,
                heap.row_tier
            ),
            (8, 32, 8, 32)
        );
        let (objects, rows) = (heap.objects.clone(), heap.rows.clone());
        let mut requests = Vec::new();
        let mut fail_second = |r| {
            requests.push(r);
            if requests.len() == 2 {
                Err(Fault::Allocation)
            } else {
                Ok(())
            }
        };
        let mut budget = Budget::default();
        assert_eq!(
            heap.stage_function(FunctionId(0), EMPTY, 0, &[], &mut budget, &mut fail_second)
                .err(),
            Some(Fault::Allocation)
        );
        assert_eq!(
            requests,
            [
                Request {
                    bytes: 16 * size_of::<Object>(),
                    moved: 8
                },
                Request {
                    bytes: 64 * size_of::<Row>(),
                    moved: 32
                }
            ]
        );
        assert_eq!(
            budget.work,
            WORK - 2 * (FUNCTION_OBJECTS + FUNCTION_ROWS) - 40
        );
        assert_eq!(
            budget.requests,
            REQUEST_BYTES - requests.iter().map(|r| r.bytes).sum::<usize>()
        );
        assert_eq!((&heap.objects, &heap.rows), (&objects, &rows));
        assert_eq!(
            (heap.object_tier, heap.row_tier, heap.functions),
            (8, 32, 0)
        );
        let staged = heap
            .stage_function(
                FunctionId(0),
                EMPTY,
                0,
                &[],
                &mut Budget::default(),
                &mut ok,
            )
            .unwrap();
        drop(staged);
        assert_eq!((&heap.objects, &heap.rows), (&objects, &rows));
        let name = units(b"new");
        let mut budget = Budget::default();
        let plan = heap
            .set(owner, owner, query(&name), &mut budget)
            .unwrap()
            .unwrap();
        assert_eq!(
            plan.prepare(Some(Key::Ascii(b"new")), EMPTY, &mut budget, &mut |_| Err(
                Fault::Allocation
            ))
            .err(),
            Some(Fault::Allocation)
        );
        assert_eq!((&heap.objects, &heap.rows), (&objects, &rows));
        let f = heap
            .stage_function(
                FunctionId(0),
                EMPTY,
                0,
                &[],
                &mut Budget::default(),
                &mut ok,
            )
            .unwrap()
            .publish();
        assert_eq!(read(&heap, f, b"name"), Ok(Some(EMPTY)));
    }

    #[test]
    fn malformed_ids_ranges_and_function_order_use_logical_bounds() {
        let (mut heap, _) = rooted();
        let object = ordinary(&mut heap, None);
        let nonexistent = ObjectId(heap.objects.len() as u16); // below requested tier
        let mut budget = Budget::default();
        assert_eq!(
            heap.get(nonexistent, object, query(&[]), &mut budget),
            Err(Fault::ObjectId)
        );
        assert_eq!(
            heap.get(object, nonexistent, query(&[]), &mut budget),
            Err(Fault::ObjectId)
        );
        assert_eq!(
            heap.stage_ordinary(Some(nonexistent), &mut budget, &mut ok)
                .err(),
            Some(Fault::ObjectId)
        );
        assert_eq!(
            put(&mut heap, object, b"x", FieldValue::Object(nonexistent)),
            Err(Fault::ObjectId)
        );
        let range = RealmRange { start: 0, end: 1 };
        assert_eq!(
            put(
                &mut heap,
                object,
                b"x",
                FieldValue::Stored(StoredValue::Realm(range))
            ),
            Err(Fault::Value)
        );
        assert_eq!(
            put(
                &mut heap,
                object,
                b"x",
                FieldValue::Stored(StoredValue::Function(FunctionId(0)))
            ),
            Err(Fault::Value)
        );
        assert_eq!(
            heap.stage_function(FunctionId(0), number(0), 0, &[], &mut budget, &mut ok)
                .err(),
            Some(Fault::Value)
        );
        assert_eq!(
            heap.stage_function(
                FunctionId(0),
                EMPTY,
                super::super::MAX_PARAMETERS + 1,
                &[],
                &mut budget,
                &mut ok
            )
            .err(),
            Some(Fault::Value)
        );
        assert_eq!(
            heap.stage_function(FunctionId(1), EMPTY, 0, &[], &mut budget, &mut ok)
                .err(),
            Some(Fault::Order)
        );
        let mut empty = Heap::default();
        assert_eq!(
            empty
                .stage_function(FunctionId(0), EMPTY, 0, &[], &mut budget, &mut ok)
                .err(),
            Some(Fault::Order)
        );
        let o = &mut heap.objects[usize::from(object.0)];
        o.head = Some(RowId(heap.rows.len() as u16)); // initialized prefix, not requested tier
        o.count = 1;
        assert_eq!(read(&heap, object, b"x"), Err(Fault::RowId));
    }

    #[test]
    fn key_hop_work_and_request_exact_edges() {
        let mut heap = Heap::default();
        let bytes = 4 * size_of::<Object>() + 32 * size_of::<Row>();
        let mut budget = Budget {
            requests: bytes - 1,
            ..Budget::default()
        };
        assert_eq!(
            heap.stage_roots(&mut budget, &mut ok).err(),
            Some(Fault::RequestLimit)
        );
        assert!(heap.objects.is_empty() && heap.rows.is_empty() && heap.roots.is_none());
        let mut budget = Budget {
            requests: bytes,
            ..Budget::default()
        };
        heap.stage_roots(&mut budget, &mut ok).unwrap().publish();
        assert_eq!(budget.requests, 0);
        assert_eq!(budget.work, WORK - 2 * (ROOT_OBJECTS + ROOT_ROWS));
        let base = ordinary(&mut heap, None);
        let mut arena = Arena::default();
        let key = [0xd800; KEY_UNITS];
        let range = retain(&mut arena, &key);
        let q = Query {
            key: &key,
            retained: arena.units(),
        };
        heap.set(base, base, q, &mut budget)
            .unwrap()
            .unwrap()
            .prepare(Some(Key::Retained(range)), EMPTY, &mut budget, &mut ok)
            .unwrap()
            .publish();
        let mut head = base;
        for _ in 1..WALK_HOPS {
            head = ordinary(&mut heap, Some(head));
        }
        let mut exact = Budget {
            work: WALK_HOPS + 1 + KEY_UNITS,
            requests: 0,
        };
        assert_eq!(heap.get(head, head, q, &mut exact), Ok(Some(EMPTY)));
        assert_eq!(exact.work, 0);
        let mut short = Budget {
            work: WALK_HOPS + KEY_UNITS,
            requests: 0,
        };
        assert_eq!(heap.get(head, head, q, &mut short), Err(Fault::WorkLimit));
        assert_eq!(
            heap.get(
                head,
                head,
                Query {
                    key: &[0; KEY_UNITS + 1],
                    ..q
                },
                &mut budget
            ),
            Err(Fault::KeyLimit)
        );
        let too_deep = ordinary(&mut heap, Some(head));
        assert_eq!(
            heap.get(too_deep, too_deep, q, &mut budget),
            Err(Fault::HopLimit)
        );
        eprintln!(
            "requested backing peak <= {BACKING_PEAK}; biggest plan = {}",
            size_of::<Append<'_, ROOT_OBJECTS, ROOT_ROWS>>()
        );
        assert!(size_of::<Append<'_, ROOT_OBJECTS, ROOT_ROWS>>() > size_of::<Write<'_>>());
    }

    #[test]
    fn one_work_budget_is_shared_across_operations() {
        let (mut heap, _) = rooted();
        let empty = ordinary(&mut heap, None);
        let q = query(&[0]);
        let mut budget = Budget::default();
        for _ in 0..WORK {
            assert_eq!(heap.get(empty, empty, q, &mut budget), Ok(None));
        }
        assert_eq!(
            heap.get(empty, empty, q, &mut budget),
            Err(Fault::WorkLimit)
        );
        assert_eq!(heap.rows.len(), ROOT_ROWS);
        assert_eq!(budget.requests, REQUEST_BYTES);
    }

    #[test]
    fn max_functions_objects_and_rows_allow_overwrite_at_capacity() {
        let (mut heap, _) = rooted();
        for i in 0..MAX_FUNCTIONS {
            heap.stage_function(
                FunctionId(i as u16),
                EMPTY,
                0,
                &[],
                &mut Budget::default(),
                &mut ok,
            )
            .unwrap()
            .publish();
        }
        assert_eq!(
            heap.stage_function(
                FunctionId(MAX_FUNCTIONS as u16),
                EMPTY,
                0,
                &[],
                &mut Budget::default(),
                &mut ok
            )
            .err(),
            Some(Fault::FunctionLimit)
        );
        assert_eq!(
            (heap.objects.len(), heap.rows.len()),
            (
                ROOT_OBJECTS + MAX_FUNCTIONS * FUNCTION_OBJECTS,
                ROOT_ROWS + MAX_FUNCTIONS * FUNCTION_ROWS
            )
        );
        let mut owners = Vec::new();
        while heap.objects.len() < OBJECTS {
            owners.push(ordinary(&mut heap, None));
        }
        assert_eq!(
            heap.stage_ordinary(None, &mut Budget::default(), &mut ok)
                .err(),
            Some(Fault::ObjectLimit)
        );
        let mut arena = Arena::default();
        let keys: Vec<_> = (0..=OWN_ROWS)
            .map(|i| retain(&mut arena, &[i as u16]))
            .collect();
        let mut names = keys
            .iter()
            .map(|&range| (range.get(arena.units()).unwrap(), range))
            .cycle();
        let q = |key| Query {
            key,
            retained: arena.units(),
        };
        for &owner in &owners {
            let mut budget = Budget::default();
            for (key, range) in names.by_ref().take(OWN_ROWS) {
                if heap.rows.len() == ROWS {
                    break;
                }
                heap.set(owner, owner, q(key), &mut budget)
                    .unwrap()
                    .unwrap()
                    .prepare(Some(Key::Retained(range)), EMPTY, &mut budget, &mut ok)
                    .unwrap()
                    .publish();
            }
            if heap.rows.len() == ROWS {
                break;
            }
            let (key, _) = names.next().unwrap();
            assert_eq!(
                heap.set(owner, owner, q(key), &mut budget).err(),
                Some(Fault::OwnRowLimit)
            );
        }
        let (len, full, spare) = (heap.rows.len(), owners[0], *owners.last().unwrap());
        assert_eq!(len, ROWS);
        let key = &[0];
        let mut budget = Budget::default();
        let p = heap
            .set(spare, spare, q(key), &mut budget)
            .unwrap()
            .unwrap();
        assert_eq!(
            p.prepare(Some(Key::Retained(keys[0])), EMPTY, &mut budget, &mut ok)
                .err(),
            Some(Fault::RowLimit)
        );
        heap.set(full, full, q(key), &mut budget)
            .unwrap()
            .unwrap()
            .prepare(None, FieldValue::Object(full), &mut budget, &mut ok)
            .unwrap()
            .publish();
        assert_eq!(
            heap.get(full, full, q(key), &mut budget),
            Ok(Some(FieldValue::Object(full)))
        );
        assert_eq!(heap.rows.len(), ROWS);
        drop(heap); // function/backlink cycles and the self edge have no owning references
        let (new_heap, _) = rooted();
        assert_eq!(
            (new_heap.objects.len(), new_heap.rows.len()),
            (ROOT_OBJECTS, ROOT_ROWS)
        );
    }
}
