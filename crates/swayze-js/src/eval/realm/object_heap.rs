//! Realm-private initial callable shape. Neither these descriptors nor the
//! intrinsic identities are guest operands yet. Only the two function creators
//! can append to this heap; no lookup, mutation, GC, or general value model.

use std::mem::size_of;

use super::{Artifact, FunctionId, FunctionOrigin, InstantiateError, MAX_FUNCTIONS, commit_rows};

const OBJECT_LIMIT: usize = 1280;
const ROW_LIMIT: usize = 4096;
const ROOT_OBJECTS: usize = 4;
const ROOT_ROWS: usize = 27;
const FUNCTION_OBJECTS: usize = 2;
const FUNCTION_ROWS: usize = 4;
pub(super) const CREATION_WORK: usize = 128 * 1024;
pub(super) const CREATION_REQUEST_BYTES: usize = 512 * 1024;

const _: () = {
    assert!(ROOT_OBJECTS + MAX_FUNCTIONS * FUNCTION_OBJECTS <= OBJECT_LIMIT);
    assert!(ROOT_ROWS + MAX_FUNCTIONS * FUNCTION_ROWS <= ROW_LIMIT);
    assert!(OBJECT_LIMIT <= u16::MAX as usize);
    assert!(ROW_LIMIT <= u16::MAX as usize);
    // Sum of all possible logical tier requests (even one-at-a-time creates).
    assert!(
        (2047 + OBJECT_LIMIT) * size_of::<Object>() + 8191 * size_of::<Row>()
            <= CREATION_REQUEST_BYTES
    );
    // Each initial node/row has two writes charged. The sum of old lengths
    // moved is bounded by its preceding geometric tiers.
    assert!(2 * (OBJECT_LIMIT + ROW_LIMIT) + 2047 + 4095 <= CREATION_WORK);
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObjectId(u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RowId(u16);

const OBJECT_PROTO: ObjectId = ObjectId(0);
const FUNCTION_PROTO: ObjectId = ObjectId(1);
const OBJECT_CTOR: ObjectId = ObjectId(2);
const FUNCTION_CTOR: ObjectId = ObjectId(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Intrinsic {
    FunctionPrototype,
    ObjectConstructor,
    FunctionConstructor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Callable {
    User(FunctionId),
    Intrinsic(Intrinsic),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitialName {
    // Absolute retained origin, not a copied spelling or an operand.
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
    UnmaterializedBuiltin,
    RestrictedAccessor,
    AnnexB,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Attributes(u8);

// No initial data property is enumerable. Accessor/Annex B barriers are not
// treated as data properties with these bits.
const WRITABLE: u8 = 1;
const CONFIGURABLE: u8 = 2;
const METHOD: Attributes = Attributes(WRITABLE | CONFIGURABLE);
const META: Attributes = Attributes(CONFIGURABLE);
const USER_PROTOTYPE: Attributes = Attributes(WRITABLE);
const FROZEN: Attributes = Attributes(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Descriptor {
    initial: Initial,
    data_attributes: Option<Attributes>,
}

const fn data(initial: Initial, attributes: Attributes) -> Descriptor {
    Descriptor {
        initial,
        data_attributes: Some(attributes),
    }
}

const BUILTIN: Descriptor = data(Initial::UnmaterializedBuiltin, METHOD);
const RESTRICTED: Descriptor = Descriptor {
    initial: Initial::RestrictedAccessor,
    data_attributes: None,
};
const ANNEX_B: Descriptor = Descriptor {
    initial: Initial::AnnexB,
    data_attributes: None,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Row {
    key: &'static str,
    descriptor: Descriptor,
    next: Option<RowId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Object {
    callable: Option<Callable>,
    prototype: Option<ObjectId>,
    complete: bool,
    head: Option<RowId>,
}

impl Object {
    const fn new(callable: Option<Callable>, prototype: Option<ObjectId>, complete: bool) -> Self {
        Self {
            callable,
            prototype,
            complete,
            head: None,
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
}

impl Heap {
    pub(super) fn origin(
        &self,
        id: FunctionId,
        functions: &[FunctionRecord],
    ) -> Option<FunctionOrigin> {
        let record = functions.get(usize::from(id.0))?;
        (self.objects.get(usize::from(record.object.0))?.callable == Some(Callable::User(id)))
            .then_some(record.origin)
    }

    fn append_object(&mut self, object: Object) {
        self.objects.push(object);
    }

    fn append_row(&mut self, owner: ObjectId, key: &'static str, descriptor: Descriptor) {
        let head = &mut self.objects[usize::from(owner.0)].head;
        let next = head.replace(RowId(self.rows.len() as u16));
        self.rows.push(Row {
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
                "constructor",
                data(Constructor(Callable::Intrinsic(ObjectConstructor)), METHOD),
            ),
            (OBJECT_PROTO, "hasOwnProperty", BUILTIN),
            (OBJECT_PROTO, "isPrototypeOf", BUILTIN),
            (OBJECT_PROTO, "propertyIsEnumerable", BUILTIN),
            (OBJECT_PROTO, "toLocaleString", BUILTIN),
            (OBJECT_PROTO, "toString", BUILTIN),
            (OBJECT_PROTO, "valueOf", BUILTIN),
            (OBJECT_PROTO, "__defineGetter__", ANNEX_B),
            (OBJECT_PROTO, "__defineSetter__", ANNEX_B),
            (OBJECT_PROTO, "__lookupGetter__", ANNEX_B),
            (OBJECT_PROTO, "__lookupSetter__", ANNEX_B),
            (OBJECT_PROTO, "__proto__", ANNEX_B),
            (
                FUNCTION_PROTO,
                "constructor",
                data(
                    Constructor(Callable::Intrinsic(FunctionConstructor)),
                    METHOD,
                ),
            ),
            (FUNCTION_PROTO, "name", data(Name(InitialName::Empty), META)),
            (FUNCTION_PROTO, "length", data(Length(0), META)),
            (FUNCTION_PROTO, "apply", BUILTIN),
            (FUNCTION_PROTO, "bind", BUILTIN),
            (FUNCTION_PROTO, "call", BUILTIN),
            (FUNCTION_PROTO, "toString", BUILTIN),
            (FUNCTION_PROTO, "arguments", RESTRICTED),
            (FUNCTION_PROTO, "caller", RESTRICTED),
            (
                OBJECT_CTOR,
                "prototype",
                data(Prototype(PrototypeValue::Object(OBJECT_PROTO)), FROZEN),
            ),
            (OBJECT_CTOR, "name", data(Name(InitialName::Object), META)),
            (OBJECT_CTOR, "length", data(Length(1), META)),
            (
                FUNCTION_CTOR,
                "prototype",
                data(
                    Prototype(PrototypeValue::Function(FunctionPrototype)),
                    FROZEN,
                ),
            ),
            (
                FUNCTION_CTOR,
                "name",
                data(Name(InitialName::Function), META),
            ),
            (FUNCTION_CTOR, "length", data(Length(1), META)),
        ];
        for (owner, key, descriptor) in rows {
            self.append_row(owner, key, descriptor);
        }
    }

    fn append_function(&mut self, id: FunctionId, origin: FunctionOrigin, formals: u8) -> ObjectId {
        use Initial::{Constructor, Length, Name, Prototype};
        let function = ObjectId(self.objects.len() as u16);
        let prototype = ObjectId(function.0 + 1);
        let callable = Callable::User(id);
        self.append_object(Object::new(Some(callable), Some(FUNCTION_PROTO), true));
        self.append_object(Object::new(None, Some(OBJECT_PROTO), true));
        let rows: [(_, _, _); FUNCTION_ROWS] = [
            (
                function,
                "name",
                data(Name(InitialName::Code(origin)), META),
            ),
            (function, "length", data(Length(formals), META)),
            (
                function,
                "prototype",
                data(Prototype(PrototypeValue::Object(prototype)), USER_PROTOTYPE),
            ),
            (
                prototype,
                "constructor",
                data(Constructor(callable), METHOD),
            ),
        ];
        for (owner, key, descriptor) in rows {
            self.append_row(owner, key, descriptor);
        }
        function
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

/// Same bounded recipe for GDI (selected origins only) and a reached creator
/// (one origin). Exclusive borrows keep all checked frontiers stable; the one
/// caller-owned origin slice and immutable artifact supply the whole batch.
pub(super) struct Creation<'a> {
    heap: &'a mut Heap,
    functions: &'a mut Vec<FunctionRecord>,
    artifact: &'a Artifact,
    origins: &'a [FunctionOrigin],
    roots: bool,
    objects: Tier,
    rows: Tier,
    records: Tier,
    recipe_work: usize,
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
        // Only the fixed producers exist: exact initial shape, no skipped or
        // duplicate allocations, and no independent identity frontier.
        let roots = functions.is_empty();
        let old_objects = heap.objects.len();
        let old_rows = heap.rows.len();
        let expected = |roots, per_function| {
            if functions.is_empty() {
                0
            } else {
                roots + per_function * functions.len()
            }
        };
        if old_objects != expected(ROOT_OBJECTS, FUNCTION_OBJECTS)
            || old_rows != expected(ROOT_ROWS, FUNCTION_ROWS)
        {
            return Err(InvalidScript);
        }
        let object_add = FUNCTION_OBJECTS * count + if roots { ROOT_OBJECTS } else { 0 };
        let row_add = FUNCTION_ROWS * count + if roots { ROOT_ROWS } else { 0 };

        // Ends fit by the checked user frontier and the compile-time formula.
        // Fixed recipes have unique keys per node, bounded own rows, backwards
        // [[Prototype]] edges, and descriptor links only to allocated slots.
        Ok(Self {
            objects: Tier::plan(old_objects, old_objects + object_add, OBJECT_LIMIT),
            rows: Tier::plan(old_rows, old_rows + row_add, ROW_LIMIT),
            records: Tier::plan(functions.len(), function_end, MAX_FUNCTIONS),
            recipe_work: 2 * (object_add + row_add), // stage/link + publish per entry
            heap,
            functions,
            artifact,
            origins,
            roots,
        })
    }

    pub(super) fn function_moves(&self) -> usize {
        self.records.moves
    }

    pub(super) fn heap_work(&self) -> usize {
        self.recipe_work + self.objects.moves + self.rows.moves
    }

    pub(super) fn request_bytes(&self) -> usize {
        self.objects.request * size_of::<Object>() + self.rows.request * size_of::<Row>()
    }

    pub(super) fn reserve(self) -> Result<Reserved<'a>, InstantiateError> {
        // Requested old+new overlap <= 2304 Object + 6144 Row + 768
        // FunctionRecord slots. No old table is copied during preparation;
        // each destination is detached, including on a later failure.
        Ok(Reserved {
            objects: self.objects.reserve()?,
            rows: self.rows.reserve()?,
            records: self.records.reserve()?,
            plan: self,
        })
    }
}

pub(super) struct Reserved<'a> {
    plan: Creation<'a>,
    objects: Option<Vec<Object>>,
    rows: Option<Vec<Row>>,
    records: Option<Vec<FunctionRecord>>,
}

impl Reserved<'_> {
    pub(super) fn publish(self) -> FunctionId {
        let Creation {
            heap,
            functions,
            artifact,
            origins,
            roots,
            ..
        } = self.plan;
        let first = FunctionId(functions.len() as u16);
        commit_rows(&mut heap.objects, std::iter::empty(), self.objects);
        commit_rows(&mut heap.rows, std::iter::empty(), self.rows);
        commit_rows(functions, std::iter::empty(), self.records);
        // All three tables now have room for their checked ends (regardless
        // of allocator surplus). Only infallible copies/pushes remain. Artifact
        // metadata and origin indices cannot change under the shared borrow.
        if roots {
            heap.append_roots();
        }
        for &origin in origins {
            let id = FunctionId(functions.len() as u16);
            let formals = artifact.bodies[usize::from(origin.body)].layout.formals;
            let object = heap.append_function(id, origin, formals);
            functions.push(FunctionRecord { origin, object });
        }
        first
    }
}

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

    fn own(heap: &Heap, object: ObjectId, key: &str) -> Option<Descriptor> {
        let mut row = heap.objects[usize::from(object.0)].head;
        while let Some(id) = row {
            let entry = &heap.rows[usize::from(id.0)];
            if entry.key == key {
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
        assert!(!heap.objects[2].complete && heap.objects[3].complete);
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
        assert_eq!(realm.artifacts[0].string_units, [108, 105, 116]);
        for (id, name, formals) in [(0, "d", 1), (1, "z", 0), (2, "inferred", 0), (3, "", 0)] {
            let id = FunctionId(id);
            let record = realm.functions[usize::from(id.0)];
            let origin = heap.origin(id, &realm.functions).unwrap();
            let object = record.object;
            let artifact = &realm.artifacts[usize::from(origin.artifact)];
            assert_eq!(artifact.initial_name(origin.body), Some(name.as_bytes()));
            assert_eq!(
                own(heap, object, "name"),
                Some(data(Initial::Name(InitialName::Code(origin)), META))
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
                Some(data(Initial::Constructor(Callable::User(id)), METHOD))
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
            realm.heap.origin(FunctionId(0), &realm.functions),
            Some(before.origin)
        );
        let forged = [FunctionRecord {
            origin: before.origin,
            object: after.object,
        }];
        assert_eq!(realm.heap.origin(FunctionId(0), &forged), None);
    }

    #[test]
    fn first_gdi_charges_the_exact_initial_recipe_before_any_publication() {
        let code = CompiledScript::compile("function f(){}").unwrap();
        // 14 declaration/conflict/selection/link credits; 7 artifact credits
        // (two tables, name-site walk and byte, creator merge body, name and
        // site copies), plus scanned/copied instructions. No old rows move.
        let work = 21 + code.instructions.len() + 2 * code.functions[0].instructions.len() + 74;
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
        execution.creation_work = Work(73);
        execution.creation_requests = initial_bytes;
        assert!(matches!(
            execution.create_function(0),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::GlobalLookupLimitExceeded
            )))
        ));
        assert_eq!(execution.lookup.0, 1);
        assert_eq!(shape(execution.heap), (0, 0, 0, 0));
        execution.creation_work = Work(74);
        execution.creation_requests = initial_bytes - 1;
        assert!(matches!(
            execution.create_function(0),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::AllocationFailed
            )))
        ));
        assert_eq!(execution.creation_work.0, 74);
        assert_eq!(shape(execution.heap), (0, 0, 0, 0));
        execution.creation_requests = initial_bytes;
        assert_eq!(execution.create_function(0).ok(), Some(FunctionId(0)));
        assert_eq!(
            (
                execution.lookup.0,
                execution.creation_work.0,
                execution.creation_requests
            ),
            (0, 0, 0)
        );
        // 6→8 objects fits the tier. 31→35 rows reserves 64 entries and
        // moves 31 old rows. The origin tier moves one old record separately.
        let before = shape(execution.heap);
        execution.lookup = Work(2);
        execution.creation_work = Work(42);
        execution.creation_requests = 64 * size_of::<Row>();
        assert!(matches!(
            execution.create_function(0),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::GlobalLookupLimitExceeded
            )))
        ));
        assert_eq!(shape(execution.heap), before);
        execution.creation_work = Work(43);
        assert_eq!(execution.create_function(0).ok(), Some(FunctionId(1)));
        assert_eq!(
            (
                execution.lookup.0,
                execution.creation_work.0,
                execution.creation_requests
            ),
            (0, 0, 0)
        );
        assert_eq!(
            (execution.heap.objects.len(), execution.heap.rows.len()),
            (8, 35)
        );
    }
}
