use std::cell::RefCell;
use std::rc::Rc;

use martite_base::{LoadLimits, MemoryProvider, Severity};
use martite_driver::{LoadOptions, load};
use martite_ir::{IrProgram, lower};
use martite_opt::{
    Changed, FnPass, OptimizeError, OptimizeOptions, PassCx, PassManager, ProgramPass,
};

struct RecordPass {
    name: &'static str,
    log: Rc<RefCell<Vec<&'static str>>>,
}

impl FnPass for RecordPass {
    fn name(&self) -> &'static str {
        self.name
    }

    fn run(&mut self, _function: &mut martite_ir::IrFn, _cx: &mut PassCx<'_>) -> Changed {
        self.log.borrow_mut().push(self.name);
        Changed::NO
    }
}

struct AlwaysChanged;

impl FnPass for AlwaysChanged {
    fn name(&self) -> &'static str {
        "always-changed"
    }

    fn run(&mut self, _function: &mut martite_ir::IrFn, _cx: &mut PassCx<'_>) -> Changed {
        Changed::YES
    }
}

struct ProgramRecord {
    log: Rc<RefCell<Vec<&'static str>>>,
}

impl ProgramPass for ProgramRecord {
    fn name(&self) -> &'static str {
        "program-record"
    }

    fn run(&mut self, _program: &mut IrProgram, _cx: &mut PassCx<'_>) -> Changed {
        self.log.borrow_mut().push("program-record");
        Changed::NO
    }
}

fn program() -> IrProgram {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", "fn main() { let value = 1 + 2 }\n");
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: LoadLimits::default(),
            edition: "1".into(),
            parse: martite_syntax::ParseOptions::default(),
        },
    );
    assert!(
        !loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "{}",
        loaded
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.render(&loaded.sources))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let checked = loaded.program.expect("checked program");
    lower(&checked, &loaded.sources).expect("lower")
}

#[test]
fn passes_run_in_registration_order_and_can_be_disabled() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut manager = PassManager::new(OptimizeOptions::default());
    manager.add_fn_pass(RecordPass {
        name: "first",
        log: log.clone(),
    });
    manager.add_program_pass(ProgramRecord { log: log.clone() });
    manager.add_fn_pass(RecordPass {
        name: "second",
        log: log.clone(),
    });

    assert!(manager.disable("first"));
    assert!(!manager.disable("missing"));
    manager.run(&mut program()).expect("program pass runs");
    assert_eq!(*log.borrow(), vec!["program-record", "second"]);
    assert!(!manager.is_enabled("first"));

    log.borrow_mut().clear();
    assert!(manager.enable("first"));
    manager.run(&mut program()).expect("all passes run");
    assert_eq!(*log.borrow(), vec!["first", "program-record", "second"]);
}

#[test]
fn fixpoint_stops_at_the_iteration_cap() {
    let mut manager = PassManager::new(OptimizeOptions::default().with_fixpoint(3));
    manager.add_fn_pass(AlwaysChanged);
    let error = manager
        .run(&mut program())
        .expect_err("cap must be enforced");
    assert!(matches!(error, OptimizeError::FixpointLimit { limit: 3 }));
}

#[test]
fn reduce_passes_are_gated() {
    struct ReducePass;

    impl FnPass for ReducePass {
        fn name(&self) -> &'static str {
            "reduce"
        }

        fn cost_policy(&self) -> martite_opt::CostPolicy {
            martite_opt::CostPolicy::Reduce
        }

        fn run(&mut self, _function: &mut martite_ir::IrFn, _cx: &mut PassCx<'_>) -> Changed {
            Changed::NO
        }
    }

    let mut manager = PassManager::new(OptimizeOptions::default());
    manager.add_fn_pass(ReducePass);
    let error = manager
        .run(&mut program())
        .expect_err("reduce must be opt-in");
    assert!(matches!(error, OptimizeError::Policy { .. }));
}
