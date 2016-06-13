#![feature(rustc_private)]

extern crate getopts;
extern crate miri;
extern crate rustc;
extern crate rustc_driver;
extern crate env_logger;
extern crate log_settings;
extern crate syntax;
#[macro_use] extern crate log;

use miri::{
    EvalContext,
    CachedMir,
    step,
    EvalError,
    Frame,
};
use rustc::session::Session;
use rustc_driver::{driver, CompilerCalls};
use rustc::ty::{TyCtxt, subst};
use rustc::mir::mir_map::MirMap;
use rustc::mir::repr::Mir;
use rustc::hir::def_id::DefId;
use rustc::hir::{map, ItemFn, Item};
use syntax::codemap::Span;

struct MiriCompilerCalls;

impl<'a> CompilerCalls<'a> for MiriCompilerCalls {
    fn build_controller(
        &mut self,
        _: &Session,
        _: &getopts::Matches
    ) -> driver::CompileController<'a> {
        let mut control = driver::CompileController::basic();

        control.after_analysis.callback = Box::new(|state| {
            state.session.abort_if_errors();

            let tcx = state.tcx.unwrap();
            let mir_map = state.mir_map.unwrap();
            let (span, mir, def_id) = get_main(tcx, mir_map);
            println!("found `main` function at: {:?}", span);

            let mut ecx = EvalContext::new(tcx, mir_map);
            let substs = tcx.mk_substs(subst::Substs::empty());
            let return_ptr = ecx.alloc_ret_ptr(mir.return_ty, substs).expect("main function should not be diverging");

            ecx.push_stack_frame(def_id, mir.span, CachedMir::Ref(mir), substs, Some(return_ptr));

            loop {
                match step(&mut ecx) {
                    Ok(true) => {}
                    Ok(false) => break,
                    // FIXME: diverging functions can end up here in some future miri
                    Err(e) => {
                        report(tcx, &ecx, e);
                        break;
                    }
                }
            }
        });

        control
    }
}

fn get_main<'a, 'b, 'tcx: 'b>(tcx: TyCtxt<'a, 'tcx, 'tcx>, mir_map: &'b MirMap<'tcx>) -> (Span, &'b Mir<'tcx>, DefId) {
    for (&id, mir) in &mir_map.map {
        if let map::Node::NodeItem(&Item { name, span, ref node, .. }) = tcx.map.get(id) {
            if let ItemFn(..) = *node {
                if name.as_str() == "main" {
                    return (span, mir, tcx.map.local_def_id(id));
                }
            }
        }
    }
    panic!("no main function found");
}

fn report(tcx: TyCtxt, ecx: &EvalContext, e: EvalError) {
    let frame = ecx.stack().last().expect("stackframe was empty");
    let block = &frame.mir.basic_blocks()[frame.next_block];
    let span = if frame.stmt < block.statements.len() {
        block.statements[frame.stmt].source_info.span
    } else {
        block.terminator().source_info.span
    };
    let mut err = tcx.sess.struct_span_err(span, &e.to_string());
    for &Frame { def_id, substs, span, .. } in ecx.stack().iter().rev() {
        // FIXME(solson): Find a way to do this without this Display impl hack.
        use rustc::util::ppaux;
        use std::fmt;
        struct Instance<'tcx>(DefId, &'tcx subst::Substs<'tcx>);
        impl<'tcx> fmt::Display for Instance<'tcx> {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                ppaux::parameterized(f, self.1, self.0, ppaux::Ns::Value, &[],
                    |tcx| Some(tcx.lookup_item_type(self.0).generics))
            }
        }
        err.span_note(span, &format!("inside call to {}", Instance(def_id, substs)));
    }
    err.emit();
}

fn main() {
    init_logger();
    let args: Vec<String> = std::env::args().collect();
    rustc_driver::run_compiler(&args, &mut MiriCompilerCalls);
}

fn init_logger() {
    const NSPACES: usize = 40;
    let format = |record: &log::LogRecord| {
        // prepend spaces to indent the final string
        let indentation = log_settings::settings().indentation;
        format!("{lvl}:{module}{depth:2}{indent:<indentation$} {text}",
            lvl = record.level(),
            module = record.location().module_path(),
            depth = indentation / NSPACES,
            indentation = indentation % NSPACES,
            indent = "",
            text = record.args())
    };

    let mut builder = env_logger::LogBuilder::new();
    builder.format(format).filter(None, log::LogLevelFilter::Info);

    if std::env::var("MIRI_LOG").is_ok() {
        builder.parse(&std::env::var("MIRI_LOG").unwrap());
    }

    builder.init().unwrap();
}
