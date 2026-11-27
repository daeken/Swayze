//! Bounded Realm objects, descriptor walks and detached row edits.

use std::mem::size_of;

use super::{
    Artifact, FunctionId, FunctionOrigin, InstantiateError, MAX_FUNCTIONS, RealmRange, StaticText,
    StoredValue, Value, Work, commit_rows,
};

const OBJECT_LIMIT: usize = 1280;
const ROW_LIMIT: usize = 4096;
const ROOT_OBJECTS: usize = 4;
const ROOT_ROWS: usize = 27;
const FUNCTION_OBJECTS: usize = 2;
const FUNCTION_ROWS: usize = 4;
const ERROR_OBJECTS: usize = 6;
const ERROR_ROWS: usize = 19;
const ERROR_FRONTIER_WORK: usize = ERROR_OBJECTS + 1;
pub(super) const PROPERTY_WORK: usize = 128 * 1024;
pub(super) const HEAP_REQUEST_BYTES: usize = 512 * 1024;

const _: () = {
    assert!(ROOT_OBJECTS + MAX_FUNCTIONS * FUNCTION_OBJECTS + ERROR_OBJECTS < OBJECT_LIMIT);
    assert!(ROOT_ROWS + MAX_FUNCTIONS * FUNCTION_ROWS + ERROR_ROWS <= ROW_LIMIT);
    assert!(OBJECT_LIMIT <= u16::MAX as usize);
    assert!(ROW_LIMIT <= u16::MAX as usize);
    assert!(size_of::<Row>() <= 48 && size_of::<Object>() <= 16);
    // Sum of all possible logical tier requests (even one-at-a-time creates).
    assert!(
        (2047 + OBJECT_LIMIT) * size_of::<Object>() + 8191 * size_of::<Row>() <= HEAP_REQUEST_BYTES
    );
    // Two writes per initial node/row, preceding geometric tier moves, and
    // at most one pack authentication per successful node append.
    assert!(
        2 * (OBJECT_LIMIT + ROW_LIMIT) + 2047 + 4095 + OBJECT_LIMIT * ERROR_FRONTIER_WORK
            <= PROPERTY_WORK
    );
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::eval) struct ObjectId(u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RowId(u16);

const OBJECT_PROTO: ObjectId = ObjectId(0);
const FUNCTION_PROTO: ObjectId = ObjectId(1);
const OBJECT_CTOR: ObjectId = ObjectId(2);
const FUNCTION_CTOR: ObjectId = ObjectId(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::eval) enum Intrinsic {
    FunctionPrototype,
    ObjectConstructor,
    FunctionConstructor,
    NativeErrorConstructor(ErrorKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::eval) enum ErrorKind {
    Error,
    ReferenceError,
    TypeError,
}

impl ErrorKind {
    const ALL: [Self; 3] = [Self::Error, Self::ReferenceError, Self::TypeError];

    const fn name(self) -> StoredValue {
        StoredValue::StaticText(match self {
            Self::Error => StaticText::ErrorName,
            Self::ReferenceError => StaticText::ReferenceErrorName,
            Self::TypeError => StaticText::TypeErrorName,
        })
    }

    const fn callable(self) -> Callable {
        Callable::Intrinsic(Intrinsic::NativeErrorConstructor(self))
    }
}

/// The checked start of the contiguous pack, wherever it was appended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ErrorRoots(ObjectId);

impl ErrorRoots {
    fn at(start: usize) -> Option<Self> {
        (start >= ROOT_OBJECTS && start.checked_add(ERROR_OBJECTS)? <= OBJECT_LIMIT)
            .then_some(Self(ObjectId(start as u16)))
    }

    const fn prototype(self, kind: ErrorKind) -> ObjectId {
        let ObjectId(start) = self.0;
        ObjectId(
            start
                + match kind {
                    ErrorKind::Error => 0,
                    ErrorKind::ReferenceError => 2,
                    ErrorKind::TypeError => 4,
                },
        )
    }

    const fn constructor(self, kind: ErrorKind) -> ObjectId {
        ObjectId(self.prototype(kind).0 + 1)
    }

    const fn ctor_parent(self, kind: ErrorKind) -> ObjectId {
        match kind {
            ErrorKind::Error => FUNCTION_PROTO,
            _ => self.constructor(ErrorKind::Error),
        }
    }

    const fn proto_parent(self, kind: ErrorKind) -> ObjectId {
        match kind {
            ErrorKind::Error => OBJECT_PROTO,
            _ => self.prototype(ErrorKind::Error),
        }
    }
}

type Callable = FunctionId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitialName {
    // Absolute retained origin for the immutable name spelling.
    Code(FunctionOrigin),
    Empty,
    Object,
    Function,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrototypeValue {
    Object(ObjectId),
    Function(Intrinsic),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Initial {
    Name(InitialName),
    Length(u8),
    Prototype(PrototypeValue),
    Constructor(Callable),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Attributes(u8);

// No initial data property is enumerable. Accessor/Annex B barriers are not
// treated as data properties with these bits.
const WRITABLE: u8 = 1;
const CONFIGURABLE: u8 = 2;
const ENUMERABLE: u8 = 4;
const METHOD: Attributes = Attributes(WRITABLE | CONFIGURABLE);
const META: Attributes = Attributes(CONFIGURABLE);
const USER_PROTOTYPE: Attributes = Attributes(WRITABLE);
const FROZEN: Attributes = Attributes(0);

const DYNAMIC: Attributes = Attributes(WRITABLE | CONFIGURABLE | ENUMERABLE);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Descriptor {
    Data(StoredValue, Attributes),
    Builtin,
    Restricted,
    AnnexB,
}

impl Descriptor {
    pub(super) fn writable(self) -> bool {
        match self {
            Self::Data(_, bits) => bits.0 & WRITABLE != 0,
            Self::Builtin => true,
            _ => false,
        }
    }
}

const fn data(initial: Initial, attributes: Attributes) -> Descriptor {
    let value = match initial {
        Initial::Name(name) => match name {
            InitialName::Code(origin) => StoredValue::CodeName(origin),
            InitialName::Empty => StoredValue::Empty,
            InitialName::Object => StoredValue::StaticText(StaticText::ObjectName),
            InitialName::Function => StoredValue::StaticText(StaticText::FunctionName),
        },
        Initial::Length(n) => StoredValue::Scalar(Value::Number(n as f64)),
        Initial::Prototype(p) => match p {
            PrototypeValue::Object(id) => StoredValue::Object(id),
            PrototypeValue::Function(tag) => StoredValue::Function(FunctionId::Intrinsic(tag)),
        },
        Initial::Constructor(id) => StoredValue::Function(id),
    };
    Descriptor::Data(value, attributes)
}

const BUILTIN: Descriptor = Descriptor::Builtin;
const RESTRICTED: Descriptor = Descriptor::Restricted;
const ANNEX_B: Descriptor = Descriptor::AnnexB;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Atom {
    Constructor,
    HasOwnProperty,
    IsPrototypeOf,
    PropertyIsEnumerable,
    ToLocaleString,
    ToString,
    ValueOf,
    DefineGetter,
    DefineSetter,
    LookupGetter,
    LookupSetter,
    LegacyProto,
    Name,
    Length,
    Apply,
    Bind,
    Call,
    Arguments,
    Caller,
    Prototype,
    Message,
}
impl Atom {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Constructor => b"constructor",
            Self::HasOwnProperty => b"hasOwnProperty",
            Self::IsPrototypeOf => b"isPrototypeOf",
            Self::PropertyIsEnumerable => b"propertyIsEnumerable",
            Self::ToLocaleString => b"toLocaleString",
            Self::ToString => b"toString",
            Self::ValueOf => b"valueOf",
            Self::DefineGetter => b"__defineGetter__",
            Self::DefineSetter => b"__defineSetter__",
            Self::LookupGetter => b"__lookupGetter__",
            Self::LookupSetter => b"__lookupSetter__",
            Self::LegacyProto => b"__proto__",
            Self::Name => b"name",
            Self::Length => b"length",
            Self::Apply => b"apply",
            Self::Bind => b"bind",
            Self::Call => b"call",
            Self::Arguments => b"arguments",
            Self::Caller => b"caller",
            Self::Prototype => b"prototype",
            Self::Message => b"message",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Key {
    Atom(Atom),
    Arena(RealmRange),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Row {
    owner: ObjectId,
    key: Key,
    descriptor: Descriptor,
    next: Option<RowId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Complete,
    Incomplete,
    ErrorData,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Object {
    callable: Option<Callable>,
    prototype: Option<ObjectId>,
    shape: Shape,
    head: Option<RowId>,
    own: u8,
}

impl Object {
    const fn new(callable: Option<Callable>, prototype: Option<ObjectId>, complete: bool) -> Self {
        Self {
            callable,
            prototype,
            shape: if complete {
                Shape::Complete
            } else {
                Shape::Incomplete
            },
            head: None,
            own: 0,
        }
    }

    const fn error(prototype: ObjectId) -> Self {
        Self {
            shape: Shape::ErrorData,
            ..Self::new(None, Some(prototype), true)
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct FunctionRecord {
    origin: FunctionOrigin,
    object: ObjectId,
}

#[derive(Default)]
pub(super) struct Heap {
    objects: Vec<Object>,
    rows: Vec<Row>,
    error_roots: Option<ErrorRoots>,
}

impl Heap {
    pub(super) fn origin(
        &self,
        id: FunctionId,
        functions: &[FunctionRecord],
    ) -> Option<FunctionOrigin> {
        let FunctionId::User(index) = id else {
            return None;
        };
        let record = functions.get(usize::from(index))?;
        (self.objects.get(usize::from(record.object.0))?.callable == Some(id))
            .then_some(record.origin)
    }

    #[cfg(test)]
    pub(super) fn test_snapshot(&self) -> [usize; 4] {
        [
            self.objects.len(),
            self.objects.capacity(),
            self.rows.len(),
            self.rows.capacity(),
        ]
    }

    pub(super) fn callable_object(
        &self,
        id: FunctionId,
        functions: &[FunctionRecord],
    ) -> Option<ObjectId> {
        let object = match id {
            FunctionId::User(index) => functions.get(usize::from(index))?.object,
            FunctionId::Intrinsic(Intrinsic::FunctionPrototype) => FUNCTION_PROTO,
            FunctionId::Intrinsic(Intrinsic::ObjectConstructor) => OBJECT_CTOR,
            FunctionId::Intrinsic(Intrinsic::FunctionConstructor) => FUNCTION_CTOR,
            FunctionId::Intrinsic(Intrinsic::NativeErrorConstructor(kind)) => {
                self.error_roots?.constructor(kind)
            }
        };
        (self.objects.get(usize::from(object.0))?.callable == Some(id)).then_some(object)
    }

    pub(super) fn is_noncallable(&self, id: ObjectId) -> bool {
        self.objects
            .get(usize::from(id.0))
            .is_some_and(|object| object.callable.is_none())
    }

    pub(super) fn lookup(
        &self,
        receiver: ObjectId,
        key: &[u16],
        retained: &[u16],
        work: &mut Work,
    ) -> Result<Lookup, InstantiateError> {
        use InstantiateError::{InvalidScript, PropertyLimitExceeded};
        let mut owner = receiver;
        for _ in 0..32 {
            work.charge(1)?;
            let object = self
                .objects
                .get(usize::from(owner.0))
                .ok_or(InvalidScript)?;
            if object.own > 64 {
                return Err(InvalidScript);
            }
            let mut next = object.head;
            for _ in 0..object.own {
                work.charge(1)?;
                let id = next.ok_or(InvalidScript)?;
                let row = self.rows.get(usize::from(id.0)).ok_or(InvalidScript)?;
                if row.owner != owner || row.next.is_some_and(|next| next.0 >= id.0) {
                    return Err(InvalidScript);
                }
                let equal = match row.key {
                    Key::Atom(atom) => {
                        key_equal(key, atom.bytes().iter().copied().map(u16::from), work)?
                    }
                    Key::Arena(range) => key_equal(
                        key,
                        range.get(retained).ok_or(InvalidScript)?.iter().copied(),
                        work,
                    )?,
                };
                if equal {
                    return Ok(Lookup::Found {
                        own: owner == receiver,
                        row: id,
                        descriptor: row.descriptor,
                    });
                }
                next = row.next;
            }
            if next.is_some() {
                return Err(InvalidScript);
            }
            if object.shape == Shape::Incomplete {
                return Ok(Lookup::Incomplete);
            }
            let Some(proto) = object.prototype else {
                return Ok(Lookup::Missing);
            };
            if proto.0 >= owner.0 {
                return Err(InvalidScript);
            }
            owner = proto;
        }
        Err(PropertyLimitExceeded)
    }

    pub(super) fn edit(
        &mut self,
        receiver: ObjectId,
        lookup: Lookup,
    ) -> Result<RowEdit<'_>, InstantiateError> {
        let (replace, attrs) = match lookup {
            Lookup::Found {
                own: true,
                row,
                descriptor: Descriptor::Data(_, attrs),
            } => (Some(row), attrs),
            Lookup::Found {
                own: true,
                row,
                descriptor: Descriptor::Builtin,
            } => (Some(row), METHOD),
            _ => (None, DYNAMIC),
        };
        let end = self.rows.len() + usize::from(replace.is_none());
        if replace.is_none() && (end > ROW_LIMIT || self.objects[usize::from(receiver.0)].own >= 64)
        {
            return Err(InstantiateError::PropertyLimitExceeded);
        }
        let tier = Tier::plan(self.rows.len(), end, ROW_LIMIT);
        Ok(RowEdit {
            heap: self,
            receiver,
            replace,
            attrs,
            tier,
        })
    }

    /// Root presence is determined by the heap, including when an Error was
    /// created before the first user function. Row counts are only a floor:
    /// ordinary property Set appends rows to this same frontier.
    fn creation_frontier(&self, functions: usize) -> Result<Frontier, InstantiateError> {
        use InstantiateError::InvalidScript;
        if functions > MAX_FUNCTIONS {
            return Err(InvalidScript);
        }
        let nodes = self.objects.len();
        let rows = self.rows.len();
        let roots = nodes == 0;
        let base_nodes = usize::from(!roots) * ROOT_OBJECTS + functions * FUNCTION_OBJECTS;
        let base_rows = usize::from(!roots) * ROOT_ROWS + functions * FUNCTION_ROWS;
        let mut check_work = 0;
        if let Some(pack) = self.error_roots {
            let start = usize::from(pack.0.0);
            let Some(first) = self.objects.get(start + ERROR_OBJECTS) else {
                return Err(InvalidScript);
            };
            // The first Error transaction published a pack and its first
            // instance together. Later user/function appends may interleave.
            if ErrorRoots::at(start) != Some(pack)
                || !(start - ROOT_OBJECTS).is_multiple_of(FUNCTION_OBJECTS)
                || start > base_nodes
                || nodes <= base_nodes + ERROR_OBJECTS
                || rows < base_rows + ERROR_ROWS
                || first.shape != Shape::ErrorData
                || first.callable.is_some()
                || !ErrorKind::ALL
                    .into_iter()
                    .any(|kind| first.prototype == Some(pack.prototype(kind)))
            {
                return Err(InvalidScript);
            }
            for kind in ErrorKind::ALL {
                let proto = self.objects[usize::from(pack.prototype(kind).0)];
                let ctor = self.objects[usize::from(pack.constructor(kind).0)];
                if proto.callable.is_some()
                    || proto.prototype != Some(pack.proto_parent(kind))
                    || proto.shape != Shape::Complete
                    || ctor.callable != Some(kind.callable())
                    || ctor.prototype != Some(pack.ctor_parent(kind))
                    || ctor.shape != Shape::Complete
                {
                    return Err(InvalidScript);
                }
            }
            check_work = ERROR_FRONTIER_WORK;
        } else if nodes != base_nodes || rows < base_rows || (roots && rows != 0) {
            return Err(InvalidScript);
        }
        Ok(Frontier {
            add_base: roots,
            check_work,
        })
    }

    fn append_object(&mut self, object: Object) {
        self.objects.push(object);
    }

    fn append_row(&mut self, owner: ObjectId, key: Key, descriptor: Descriptor) {
        let object = &mut self.objects[usize::from(owner.0)];
        let next = object.head.replace(RowId(self.rows.len() as u16));
        object.own += 1;
        self.rows.push(Row {
            owner,
            key,
            descriptor,
            next,
        });
    }

    fn append_roots(&mut self) {
        use Initial::{Constructor, Length, Name, Prototype};
        use Intrinsic::{FunctionConstructor, FunctionPrototype, ObjectConstructor};
        for object in [
            Object::new(None, None, true),
            Object::new(
                Some(Callable::Intrinsic(FunctionPrototype)),
                Some(OBJECT_PROTO),
                true,
            ),
            Object::new(
                Some(Callable::Intrinsic(ObjectConstructor)),
                Some(FUNCTION_PROTO),
                false,
            ),
            Object::new(
                Some(Callable::Intrinsic(FunctionConstructor)),
                Some(FUNCTION_PROTO),
                true,
            ),
        ] {
            self.append_object(object);
        }
        let rows: [(_, _, _); ROOT_ROWS] = [
            (
                OBJECT_PROTO,
                Atom::Constructor,
                data(Constructor(Callable::Intrinsic(ObjectConstructor)), METHOD),
            ),
            (OBJECT_PROTO, Atom::HasOwnProperty, BUILTIN),
            (OBJECT_PROTO, Atom::IsPrototypeOf, BUILTIN),
            (OBJECT_PROTO, Atom::PropertyIsEnumerable, BUILTIN),
            (OBJECT_PROTO, Atom::ToLocaleString, BUILTIN),
            (OBJECT_PROTO, Atom::ToString, BUILTIN),
            (OBJECT_PROTO, Atom::ValueOf, BUILTIN),
            (OBJECT_PROTO, Atom::DefineGetter, ANNEX_B),
            (OBJECT_PROTO, Atom::DefineSetter, ANNEX_B),
            (OBJECT_PROTO, Atom::LookupGetter, ANNEX_B),
            (OBJECT_PROTO, Atom::LookupSetter, ANNEX_B),
            (OBJECT_PROTO, Atom::LegacyProto, ANNEX_B),
            (
                FUNCTION_PROTO,
                Atom::Constructor,
                data(
                    Constructor(Callable::Intrinsic(FunctionConstructor)),
                    METHOD,
                ),
            ),
            (
                FUNCTION_PROTO,
                Atom::Name,
                data(Name(InitialName::Empty), META),
            ),
            (FUNCTION_PROTO, Atom::Length, data(Length(0), META)),
            (FUNCTION_PROTO, Atom::Apply, BUILTIN),
            (FUNCTION_PROTO, Atom::Bind, BUILTIN),
            (FUNCTION_PROTO, Atom::Call, BUILTIN),
            (FUNCTION_PROTO, Atom::ToString, BUILTIN),
            (FUNCTION_PROTO, Atom::Arguments, RESTRICTED),
            (FUNCTION_PROTO, Atom::Caller, RESTRICTED),
            (
                OBJECT_CTOR,
                Atom::Prototype,
                data(Prototype(PrototypeValue::Object(OBJECT_PROTO)), FROZEN),
            ),
            (
                OBJECT_CTOR,
                Atom::Name,
                data(Name(InitialName::Object), META),
            ),
            (OBJECT_CTOR, Atom::Length, data(Length(1), META)),
            (
                FUNCTION_CTOR,
                Atom::Prototype,
                data(
                    Prototype(PrototypeValue::Function(FunctionPrototype)),
                    FROZEN,
                ),
            ),
            (
                FUNCTION_CTOR,
                Atom::Name,
                data(Name(InitialName::Function), META),
            ),
            (FUNCTION_CTOR, Atom::Length, data(Length(1), META)),
        ];
        for (owner, key, descriptor) in rows {
            self.append_row(owner, Key::Atom(key), descriptor);
        }
    }

    fn append_error_roots(&mut self, pack: ErrorRoots) {
        use Initial::{Constructor, Length, Prototype};
        for kind in ErrorKind::ALL {
            let prototype = pack.prototype(kind);
            let ctor = pack.constructor(kind);
            self.append_object(Object::new(None, Some(pack.proto_parent(kind)), true));
            self.append_object(Object::new(
                Some(kind.callable()),
                Some(pack.ctor_parent(kind)),
                true,
            ));
            for (owner, key, descriptor) in [
                (
                    prototype,
                    Atom::Constructor,
                    data(Constructor(kind.callable()), METHOD),
                ),
                (prototype, Atom::Name, Descriptor::Data(kind.name(), METHOD)),
                (
                    prototype,
                    Atom::Message,
                    Descriptor::Data(StoredValue::Empty, METHOD),
                ),
                (ctor, Atom::Name, Descriptor::Data(kind.name(), META)),
                (ctor, Atom::Length, data(Length(1), META)),
                (
                    ctor,
                    Atom::Prototype,
                    data(Prototype(PrototypeValue::Object(prototype)), FROZEN),
                ),
            ] {
                self.append_row(owner, Key::Atom(key), descriptor);
            }
            if kind == ErrorKind::Error {
                self.append_row(prototype, Key::Atom(Atom::ToString), BUILTIN);
            }
        }
    }

    fn append_function(
        &mut self,
        id: FunctionId,
        origin: FunctionOrigin,
        formals: u8,
        named: bool,
    ) -> ObjectId {
        use Initial::{Constructor, Length, Name, Prototype};
        let function = ObjectId(self.objects.len() as u16);
        let prototype = ObjectId(function.0 + 1);
        let callable = id;
        self.append_object(Object::new(Some(callable), Some(FUNCTION_PROTO), true));
        self.append_object(Object::new(None, Some(OBJECT_PROTO), true));
        let rows: [(_, _, _); FUNCTION_ROWS] = [
            (
                function,
                Atom::Name,
                data(
                    Name(if named {
                        InitialName::Code(origin)
                    } else {
                        InitialName::Empty
                    }),
                    META,
                ),
            ),
            (function, Atom::Length, data(Length(formals), META)),
            (
                function,
                Atom::Prototype,
                data(Prototype(PrototypeValue::Object(prototype)), USER_PROTOTYPE),
            ),
            (
                prototype,
                Atom::Constructor,
                data(Constructor(callable), METHOD),
            ),
        ];
        for (owner, key, descriptor) in rows {
            self.append_row(owner, Key::Atom(key), descriptor);
        }
        function
    }
}

pub(super) enum Lookup {
    Found {
        own: bool,
        row: RowId,
        descriptor: Descriptor,
    },
    Missing,
    Incomplete,
}

fn key_equal(
    key: &[u16],
    other: impl ExactSizeIterator<Item = u16>,
    work: &mut Work,
) -> Result<bool, InstantiateError> {
    if key.len() != other.len() {
        return Ok(false);
    }
    for (&a, b) in key.iter().zip(other) {
        work.charge(1)?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) struct RowEdit<'a> {
    heap: &'a mut Heap,
    receiver: ObjectId,
    replace: Option<RowId>,
    attrs: Attributes,
    tier: Tier,
}

impl<'a> RowEdit<'a> {
    pub(super) fn inserts(&self) -> bool {
        self.replace.is_none()
    }
    pub(super) fn work(&self) -> usize {
        self.tier.moves + if self.inserts() { 4 } else { 2 }
    }
    pub(super) fn request_bytes(&self) -> usize {
        self.tier.request * size_of::<Row>()
    }
    pub(super) fn reserve(self) -> Result<ReservedEdit<'a>, InstantiateError> {
        let growth = self.tier.reserve()?;
        Ok(ReservedEdit { edit: self, growth })
    }
}

pub(super) struct ReservedEdit<'a> {
    edit: RowEdit<'a>,
    growth: Option<Vec<Row>>,
}

impl ReservedEdit<'_> {
    pub(super) fn publish(self, key: Option<RealmRange>, value: StoredValue) {
        let RowEdit {
            heap,
            receiver,
            replace,
            attrs,
            ..
        } = self.edit;
        let descriptor = Descriptor::Data(value, attrs);
        if let Some(row) = replace {
            heap.rows[usize::from(row.0)].descriptor = descriptor;
        } else {
            commit_rows(&mut heap.rows, std::iter::empty(), self.growth);
            heap.append_row(
                receiver,
                Key::Arena(key.expect("reserved insertion key")),
                descriptor,
            );
        }
    }
}

#[derive(Clone, Copy)]
struct Tier {
    request: usize,
    moves: usize,
}

impl Tier {
    fn plan(old: usize, end: usize, limit: usize) -> Self {
        let capacity = |len: usize| {
            if len == 0 {
                0
            } else {
                len.next_power_of_two().min(limit)
            }
        };
        let request = capacity(end);
        if request > capacity(old) {
            Self {
                request,
                moves: old,
            }
        } else {
            Self {
                request: 0,
                moves: 0,
            }
        }
    }

    fn reserve<T>(self) -> Result<Option<Vec<T>>, InstantiateError> {
        if self.request == 0 {
            return Ok(None);
        }
        let mut rows = Vec::new();
        rows.try_reserve_exact(self.request)
            .map_err(|_| InstantiateError::AllocationFailed)?;
        Ok(Some(rows))
    }
}

struct Frontier {
    add_base: bool,
    check_work: usize,
}

/// Shared append budget and detached reserves for both namespace/origin
/// creation and Error materialization; an append cannot borrow guest data.
struct HeapAppend {
    add_base: bool,
    objects: Tier,
    rows: Tier,
    recipe_work: usize,
}

impl HeapAppend {
    fn plan(
        heap: &Heap,
        frontier: Frontier,
        nodes: usize,
        rows: usize,
    ) -> Result<Self, InstantiateError> {
        use InstantiateError::PropertyLimitExceeded;
        let nodes = nodes + if frontier.add_base { ROOT_OBJECTS } else { 0 };
        let rows = rows + if frontier.add_base { ROOT_ROWS } else { 0 };
        let old_nodes = heap.objects.len();
        let old_rows = heap.rows.len();
        let node_end = old_nodes
            .checked_add(nodes)
            .filter(|&end| end <= OBJECT_LIMIT)
            .ok_or(PropertyLimitExceeded)?;
        let row_end = old_rows
            .checked_add(rows)
            .filter(|&end| end <= ROW_LIMIT)
            .ok_or(PropertyLimitExceeded)?;
        Ok(Self {
            add_base: frontier.add_base,
            objects: Tier::plan(old_nodes, node_end, OBJECT_LIMIT),
            rows: Tier::plan(old_rows, row_end, ROW_LIMIT),
            recipe_work: frontier.check_work + 2 * (nodes + rows), // stage/link + publish
        })
    }

    fn work(&self) -> usize {
        self.recipe_work + self.objects.moves + self.rows.moves
    }

    fn bytes(&self) -> usize {
        self.objects.request * size_of::<Object>() + self.rows.request * size_of::<Row>()
    }

    fn reserve(
        &self,
        before_reserve: &mut impl FnMut(usize) -> Result<(), InstantiateError>,
    ) -> Result<ReservedHeap, InstantiateError> {
        // Requested old+new overlap <= 2304 Object + 6144 Row slots. No
        // copying during preparation; a second reserve failure drops the first.
        before_reserve(0)?;
        let objects = self.objects.reserve()?;
        before_reserve(1)?;
        let rows = self.rows.reserve()?;
        Ok(ReservedHeap {
            add_base: self.add_base,
            objects,
            rows,
        })
    }
}

struct ReservedHeap {
    add_base: bool,
    objects: Option<Vec<Object>>,
    rows: Option<Vec<Row>>,
}

impl ReservedHeap {
    fn publish(self, heap: &mut Heap) {
        commit_rows(&mut heap.objects, std::iter::empty(), self.objects);
        commit_rows(&mut heap.rows, std::iter::empty(), self.rows);
        if self.add_base {
            heap.append_roots();
        }
    }
}

/// One instance and (on first use) the entire intrinsic pack. No function
/// origins, dynamic properties or Strings are staged by this recipe.
pub(super) struct ErrorCreation<'a> {
    heap: &'a mut Heap,
    append: HeapAppend,
    new_pack: Option<ErrorRoots>,
    prototype: ObjectId,
    id: ObjectId,
}

impl<'a> ErrorCreation<'a> {
    pub(super) fn plan(
        heap: &'a mut Heap,
        functions: usize,
        kind: ErrorKind,
    ) -> Result<Self, InstantiateError> {
        let frontier = heap.creation_frontier(functions)?;
        let needs_pack = heap.error_roots.is_none();
        let nodes = 1 + usize::from(needs_pack) * ERROR_OBJECTS;
        let rows = usize::from(needs_pack) * ERROR_ROWS;
        let start = heap.objects.len() + usize::from(frontier.add_base) * ROOT_OBJECTS;
        let append = HeapAppend::plan(heap, frontier, nodes, rows)?;
        let pack = match heap.error_roots {
            Some(pack) => pack,
            None => ErrorRoots::at(start).ok_or(InstantiateError::InvalidScript)?,
        };
        Ok(Self {
            heap,
            append,
            new_pack: needs_pack.then_some(pack),
            prototype: pack.prototype(kind),
            id: ObjectId((start + nodes - 1) as u16),
        })
    }

    pub(super) fn heap_work(&self) -> usize {
        self.append.work()
    }
    pub(super) fn request_bytes(&self) -> usize {
        self.append.bytes()
    }

    pub(super) fn reserve_with(
        self,
        mut before_reserve: impl FnMut(usize) -> Result<(), InstantiateError>,
    ) -> Result<ReservedError<'a>, InstantiateError> {
        let growth = self.append.reserve(&mut before_reserve)?;
        Ok(ReservedError { plan: self, growth })
    }
}

pub(super) struct ReservedError<'a> {
    plan: ErrorCreation<'a>,
    growth: ReservedHeap,
}

impl ReservedError<'_> {
    pub(super) fn publish(self) -> ObjectId {
        let ErrorCreation {
            heap,
            new_pack,
            prototype,
            id,
            ..
        } = self.plan;
        self.growth.publish(heap);
        if let Some(pack) = new_pack {
            heap.append_error_roots(pack);
        }
        heap.append_object(Object::error(prototype));
        if new_pack.is_some() {
            heap.error_roots = new_pack;
        }
        id
    }
}

/// Same bounded recipe for GDI (selected origins only) and a reached creator
/// (one origin). Exclusive borrows keep all checked frontiers stable; the one
/// caller-owned origin slice and immutable artifact supply the whole batch.
pub(super) struct Creation<'a> {
    heap: &'a mut Heap,
    functions: &'a mut Vec<FunctionRecord>,
    artifact: &'a Artifact,
    origins: &'a [FunctionOrigin],
    append: HeapAppend,
    records: Tier,
}

impl<'a> Creation<'a> {
    pub(super) fn plan(
        heap: &'a mut Heap,
        functions: &'a mut Vec<FunctionRecord>,
        artifact_id: u8,
        artifact: &'a Artifact,
        origins: &'a [FunctionOrigin],
    ) -> Result<Self, InstantiateError> {
        use InstantiateError::{FunctionLimitExceeded, InvalidScript};
        let count = origins.len();
        if count == 0 || count > super::MAX_DECLARATIONS {
            return Err(InvalidScript);
        }
        for &origin in origins {
            if origin.artifact != artifact_id || artifact.initial_name(origin.body).is_none() {
                return Err(InvalidScript);
            }
        }
        let function_end = functions
            .len()
            .checked_add(count)
            .filter(|&end| end <= MAX_FUNCTIONS)
            .ok_or(FunctionLimitExceeded)?;
        let frontier = heap.creation_frontier(functions.len())?;
        let append = HeapAppend::plan(
            heap,
            frontier,
            FUNCTION_OBJECTS * count,
            FUNCTION_ROWS * count,
        )?;
        Ok(Self {
            append,
            records: Tier::plan(functions.len(), function_end, MAX_FUNCTIONS),
            heap,
            functions,
            artifact,
            origins,
        })
    }

    pub(super) fn function_moves(&self) -> usize {
        self.records.moves
    }

    pub(super) fn heap_work(&self) -> usize {
        self.append.work()
    }

    pub(super) fn request_bytes(&self) -> usize {
        self.append.bytes()
    }

    pub(super) fn reserve(self) -> Result<Reserved<'a>, InstantiateError> {
        self.reserve_with(|_| Ok(()))
    }

    pub(super) fn reserve_with(
        self,
        mut before_reserve: impl FnMut(usize) -> Result<(), InstantiateError>,
    ) -> Result<Reserved<'a>, InstantiateError> {
        let growth = self.append.reserve(&mut before_reserve)?;
        // At most 768 FunctionRecord slots old+new, also detached.
        before_reserve(2)?;
        let records = self.records.reserve()?;
        Ok(Reserved {
            growth,
            records,
            plan: self,
        })
    }
}

pub(super) struct Reserved<'a> {
    plan: Creation<'a>,
    growth: ReservedHeap,
    records: Option<Vec<FunctionRecord>>,
}

impl Reserved<'_> {
    pub(super) fn publish(self) -> FunctionId {
        let Creation {
            heap,
            functions,
            artifact,
            origins,
            ..
        } = self.plan;
        let first = FunctionId::User(functions.len() as u16);
        self.growth.publish(heap);
        commit_rows(functions, std::iter::empty(), self.records);
        // All three tables now have room for their checked ends (regardless
        // of allocator surplus). Only infallible copies/pushes remain. Artifact
        // metadata and origin indices cannot change under the shared borrow.
        for &origin in origins {
            let id = FunctionId::User(functions.len() as u16);
            let formals = artifact.bodies[usize::from(origin.body)].layout.formals;
            let object = heap.append_function(
                id,
                origin,
                formals,
                !artifact.initial_name(origin.body).unwrap().is_empty(),
            );
            functions.push(FunctionRecord { origin, object });
        }
        first
    }
}

#[cfg(test)]
mod error_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::realm::{
        Bindings, CompiledScript, Completion, EngineFault, EvalError, Realm, Stop, Work,
    };

    fn shape(heap: &Heap) -> (usize, usize, usize, usize) {
        (
            heap.objects.len(),
            heap.rows.len(),
            heap.objects.capacity(),
            heap.rows.capacity(),
        )
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn requested_heap_layout_on_arm64() {
        assert_eq!(
            (
                size_of::<Object>(),
                size_of::<Row>(),
                size_of::<FunctionRecord>()
            ),
            (14, 40, 6)
        );
    }

    fn own(heap: &Heap, object: ObjectId, key: &str) -> Option<Descriptor> {
        let mut row = heap.objects[usize::from(object.0)].head;
        while let Some(id) = row {
            let entry = &heap.rows[usize::from(id.0)];
            if matches!(entry.key, Key::Atom(atom) if atom.bytes() == key.as_bytes()) {
                return Some(entry.descriptor);
            }
            assert!(entry.next.is_none_or(|next| next.0 < id.0));
            row = entry.next;
        }
        None
    }

    #[test]
    fn selected_hoists_and_reached_expressions_share_real_roots_and_names() {
        let mut realm = Realm::new();
        let code = CompiledScript::compile(
            "function d(a,b){} var inferred = function(){}; function d(a){var x; return 'lit';} (function(){}); function z(){}",
        ).unwrap();
        assert_eq!(
            realm.instantiate(&code).unwrap().execute(),
            Ok(Completion::Normal)
        );
        drop(code);
        assert_eq!(
            (
                realm.functions.len(),
                realm.heap.objects.len(),
                realm.heap.rows.len()
            ),
            (4, 12, 43)
        );
        let heap = &realm.heap;
        assert_eq!(heap.objects[0].prototype, None);
        assert_eq!(heap.objects[1].prototype, Some(OBJECT_PROTO));
        assert_eq!(heap.objects[2].prototype, Some(FUNCTION_PROTO));
        assert_eq!(heap.objects[3].prototype, Some(FUNCTION_PROTO));
        assert_eq!(heap.objects[2].shape, Shape::Incomplete);
        assert_eq!(heap.objects[3].shape, Shape::Complete);
        for (prototype, intrinsic) in [
            (OBJECT_PROTO, Intrinsic::ObjectConstructor),
            (FUNCTION_PROTO, Intrinsic::FunctionConstructor),
        ] {
            assert_eq!(
                own(heap, prototype, "constructor"),
                Some(data(
                    Initial::Constructor(Callable::Intrinsic(intrinsic)),
                    METHOD
                ))
            );
        }
        assert_eq!(
            own(heap, OBJECT_CTOR, "prototype"),
            Some(data(
                Initial::Prototype(PrototypeValue::Object(OBJECT_PROTO)),
                FROZEN
            ))
        );
        assert_eq!(
            own(heap, FUNCTION_CTOR, "prototype"),
            Some(data(
                Initial::Prototype(PrototypeValue::Function(Intrinsic::FunctionPrototype)),
                FROZEN
            ))
        );
        assert_eq!(own(heap, FUNCTION_PROTO, "caller"), Some(RESTRICTED));
        assert_eq!(own(heap, FUNCTION_PROTO, "call"), Some(BUILTIN));
        assert_eq!(own(heap, OBJECT_PROTO, "__proto__"), Some(ANNEX_B));
        assert_eq!(own(heap, FUNCTION_PROTO, "prototype"), None);
        assert_eq!(realm.artifacts[0].literal_units(), [108, 105, 116]);
        for (index, name, formals) in [(0, "d", 1), (1, "z", 0), (2, "inferred", 0), (3, "", 0)] {
            let id = FunctionId::User(index);
            let record = realm.functions[usize::from(index)];
            let origin = heap.origin(id, &realm.functions).unwrap();
            let object = record.object;
            let artifact = &realm.artifacts[usize::from(origin.artifact)];
            assert_eq!(
                artifact.initial_name(origin.body),
                Some(name.encode_utf16().collect::<Vec<_>>().as_slice())
            );
            assert_eq!(
                own(heap, object, "name"),
                Some(data(
                    Initial::Name(if name.is_empty() {
                        InitialName::Empty
                    } else {
                        InitialName::Code(origin)
                    }),
                    META
                ))
            );
            assert_eq!(
                own(heap, object, "length"),
                Some(data(Initial::Length(formals), META))
            );
            assert_eq!(
                heap.objects[usize::from(object.0)].prototype,
                Some(FUNCTION_PROTO)
            );
            let proto = ObjectId(object.0 + 1);
            assert_eq!(
                own(heap, object, "prototype"),
                Some(data(
                    Initial::Prototype(PrototypeValue::Object(proto)),
                    USER_PROTOTYPE
                ))
            );
            assert_eq!(
                heap.objects[usize::from(proto.0)].prototype,
                Some(OBJECT_PROTO)
            );
            assert_eq!(
                own(heap, proto, "constructor"),
                Some(data(Initial::Constructor(id), METHOD))
            );
        }
    }

    #[test]
    fn roots_only_appear_on_committed_creation_and_old_code_is_not_relinked() {
        let mut realm = Realm::new();
        let skipped = CompiledScript::compile("false && function(){};").unwrap();
        drop(realm.instantiate(&skipped).unwrap());
        assert_eq!(
            realm.instantiate(&skipped).unwrap().execute(),
            Ok(Completion::Normal)
        );
        let no_body = CompiledScript::compile("1;").unwrap();
        assert_eq!(
            realm.instantiate(&no_body).unwrap().execute(),
            Ok(Completion::Normal)
        );
        assert_eq!(shape(&realm.heap), (0, 0, 0, 0));
        let first = CompiledScript::compile("function one(){}").unwrap();
        drop(realm.instantiate(&first).unwrap());
        let before = realm.functions[0];
        drop(realm.instantiate(&first).unwrap());
        let after = realm.functions[1];
        assert_ne!(before.object, after.object);
        assert_eq!((before.origin.artifact, after.origin.artifact), (2, 3));
        assert_eq!((realm.heap.objects.len(), realm.heap.rows.len()), (8, 35));
        assert_eq!(
            realm.heap.origin(FunctionId::User(0), &realm.functions),
            Some(before.origin)
        );
        let forged = [FunctionRecord {
            origin: before.origin,
            object: after.object,
        }];
        assert_eq!(realm.heap.origin(FunctionId::User(0), &forged), None);
    }

    #[test]
    fn first_gdi_charges_the_exact_initial_recipe_before_any_publication() {
        let code = CompiledScript::compile("function f(){}").unwrap();
        // 14 declaration/conflict/selection/link credits; 8 artifact credits
        // (two tables, name-site walk and byte, creator merge body, name and
        // site copies), plus scanned/copied instructions. No old rows move.
        let work = 22 + code.instructions.len() + 2 * code.functions[0].instructions.len() + 74;
        let mut realm = Realm::new();
        assert_eq!(
            realm.instantiate_with_work(&code, work - 1).unwrap_err(),
            InstantiateError::WorkLimitExceeded
        );
        assert_eq!(shape(&realm.heap), (0, 0, 0, 0));
        assert_eq!(realm.functions.capacity(), 0);
        assert_eq!(realm.artifacts.capacity(), 0);
        assert_eq!(realm.state.object.vars.capacity(), 0);
        drop(realm.instantiate_with_work(&code, work).unwrap());
        assert_eq!(realm.heap.rows.len(), 31);
    }

    #[test]
    fn reached_creation_charges_the_actual_row_frontier_after_insertion() {
        let setup = CompiledScript::compile("function f(){} f.xyz = 7;").unwrap();
        let expression = CompiledScript::compile("(function(){});").unwrap();
        let mut realm = Realm::new();
        assert_eq!(
            realm.instantiate(&setup).unwrap().execute(),
            Ok(Completion::Normal)
        );
        let mut ready = realm.instantiate(&expression).unwrap();
        let mut execution = ready.execution();
        assert_eq!(
            (execution.heap.objects.len(), execution.heap.rows.len()),
            (6, 32)
        );
        let snapshot = |execution: &crate::eval::realm::Execution<'_, '_>| {
            let first = execution.functions[0];
            (
                shape(execution.heap),
                execution.functions.len(),
                execution.functions.capacity(),
                first.origin,
                first.object,
            )
        };
        let before = snapshot(&execution);
        let request = 64 * size_of::<Row>();
        // Two nodes/four rows cost 12 writes; row growth moves all 32 old
        // rows, not just the 31 initial rows. Object growth isn't needed;
        // the origin write plus one moved record use the other ledger.
        execution.lookup = Work(2);
        execution.property_work = Work(43);
        execution.heap_requests = request;
        assert!(matches!(
            execution.create_function(0),
            Err(Stop::Engine(EngineFault::PropertyWorkLimitExceeded))
        ));
        assert_eq!(snapshot(&execution), before);
        assert_eq!(
            (
                execution.lookup.0,
                execution.property_work.0,
                execution.heap_requests
            ),
            (2, 43, request)
        );

        execution.property_work = Work(44);
        execution.heap_requests = request - 1;
        assert!(matches!(
            execution.create_function(0),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::AllocationFailed
            )))
        ));
        assert_eq!(snapshot(&execution), before);
        assert_eq!(
            (
                execution.lookup.0,
                execution.property_work.0,
                execution.heap_requests
            ),
            (2, 44, request - 1)
        );

        execution.lookup = Work(1);
        execution.heap_requests = request;
        assert!(matches!(
            execution.create_function(0),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::GlobalLookupLimitExceeded
            )))
        ));
        assert_eq!(snapshot(&execution), before);
        assert_eq!(
            (
                execution.lookup.0,
                execution.property_work.0,
                execution.heap_requests
            ),
            (1, 44, request)
        );

        execution.lookup = Work(2);
        assert_eq!(execution.create_function(0).ok(), Some(FunctionId::User(1)));
        assert_eq!(
            (
                execution.lookup.0,
                execution.property_work.0,
                execution.heap_requests
            ),
            (0, 0, 0)
        );
        assert_eq!(
            (
                execution.heap.objects.len(),
                execution.heap.rows.len(),
                execution.functions.len()
            ),
            (8, 36, 2)
        );
        drop(ready);
        let observer = CompiledScript::compile("if (f.xyz !== 7) fail;").unwrap();
        assert_eq!(
            realm.instantiate(&observer).unwrap().execute(),
            Ok(Completion::Normal)
        );
    }

    #[test]
    fn creation_request_and_work_edges_are_exact_and_failed_staging_is_detached() {
        let code = CompiledScript::compile("(function(){});").unwrap();
        let mut realm = Realm::new();
        let mut ready = realm.instantiate(&code).unwrap();
        let mut execution = ready.execution();
        let origin = FunctionOrigin {
            artifact: 0,
            body: 0,
        };
        let plan = Creation::plan(
            execution.heap,
            execution.functions,
            0,
            &execution.callable.artifacts[0],
            std::slice::from_ref(&origin),
        )
        .unwrap();
        assert_eq!(plan.heap_work(), 74);
        assert_eq!(plan.function_moves(), 0);
        let initial_bytes = 8 * size_of::<Object>() + 32 * size_of::<Row>();
        assert_eq!(plan.request_bytes(), initial_bytes);
        drop(plan.reserve().unwrap());
        assert_eq!(shape(execution.heap), (0, 0, 0, 0));
        assert_eq!(execution.functions.capacity(), 0);
        execution.lookup = Work(1);
        execution.property_work = Work(73);
        execution.heap_requests = initial_bytes;
        assert!(matches!(
            execution.create_function(0),
            Err(Stop::Engine(EngineFault::PropertyWorkLimitExceeded))
        ));
        assert_eq!(execution.lookup.0, 1);
        assert_eq!(shape(execution.heap), (0, 0, 0, 0));
        execution.property_work = Work(74);
        execution.heap_requests = initial_bytes - 1;
        assert!(matches!(
            execution.create_function(0),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::AllocationFailed
            )))
        ));
        assert_eq!(execution.property_work.0, 74);
        assert_eq!(shape(execution.heap), (0, 0, 0, 0));
        execution.heap_requests = initial_bytes;
        assert_eq!(execution.create_function(0).ok(), Some(FunctionId::User(0)));
        assert_eq!(
            (
                execution.lookup.0,
                execution.property_work.0,
                execution.heap_requests
            ),
            (0, 0, 0)
        );
        // 6→8 objects fits the tier. 31→35 rows reserves 64 entries and
        // moves 31 old rows. The origin tier moves one old record separately.
        let before = shape(execution.heap);
        execution.lookup = Work(2);
        execution.property_work = Work(42);
        execution.heap_requests = 64 * size_of::<Row>();
        assert!(matches!(
            execution.create_function(0),
            Err(Stop::Engine(EngineFault::PropertyWorkLimitExceeded))
        ));
        assert_eq!(shape(execution.heap), before);
        execution.property_work = Work(43);
        assert_eq!(execution.create_function(0).ok(), Some(FunctionId::User(1)));
        assert_eq!(
            (
                execution.lookup.0,
                execution.property_work.0,
                execution.heap_requests
            ),
            (0, 0, 0)
        );
        assert_eq!(
            (execution.heap.objects.len(), execution.heap.rows.len()),
            (8, 35)
        );
    }
}
