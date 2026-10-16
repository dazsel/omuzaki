//! Where a closure may appear.
//!
//! Rhai's size limits count the values held in arrays and maps, but not what a closure captures or a function pointer
//! holds, so a script could keep any number of large strings alive in closures without reaching a limit
//! (`fs.push(|| big)` in a loop). A function pointer can only come from a closure here (`Fn` and `call` are
//! disabled), so the sandbox lets a closure exist only as an argument of a call that uses it and lets go of it:
//! `try_op` and the array helpers that take a function. Anywhere else (a variable, an array or map, a `return`, a block,
//! another closure's value, an argument of a script function) the script is refused when it is compiled, by the run and
//! by the check alike. A closure is then dropped when its call returns, and what it captured is the variables the
//! script already names.

use rhai::{AST, ASTNode, Dynamic, Expr, FnPtr, Position, Stmt};

/// The functions a closure may be passed to: `try_op`, and the array helpers of Rhai's `BasicArrayPackage` that call
/// the function and drop it.
const HOLDERS: [&str; 14] = [
    "try_op",
    "for_each",
    "map",
    "filter",
    "find",
    "find_map",
    "some",
    "all",
    "dedup",
    "reduce",
    "reduce_rev",
    "sort",
    "drain",
    "retain",
];

/// The name Rhai gives the call that binds a closure's captured variables.
const CURRY: &str = "curry";

/// A closure in a place that could keep it alive, and what to tell the author.
pub(crate) struct Stray {
    /// Where the closure starts (none when it is the script's own function that is at fault).
    pub at: Position,
    /// What is wrong, in a sentence that carries the hint.
    pub message: String,
}

/// The first closure in `ast` that is not a direct argument of one of the [`HOLDERS`], or a script function that
/// shadows one of them while the script uses closures.
pub(crate) fn stray(ast: &AST) -> Option<Stray> {
    let mut found = None;
    ast.walk(&mut |nodes: &[ASTNode<'_>]| {
        let Some(ASTNode::Expr(Expr::DynamicConstant(value, at))) = nodes.last() else { return true };
        if !is_closure(value) || held_by_a_call(&nodes[..nodes.len() - 1]) {
            return true;
        }
        found = Some(Stray {
            at: *at,
            message: format!(
                "a closure can only be passed straight to `try_op` or to an array function ({}); it can't be kept in \
                 a variable, an array or a map, or returned",
                HOLDERS[1..].join(", ")
            ),
        });
        false
    });
    if found.is_some() {
        return found;
    }
    let shadowing = ast.iter_functions().find(|function| HOLDERS.contains(&function.name));
    let closures = ast.iter_functions().any(|function| function.name.starts_with("anon$"));
    shadowing.filter(|_| closures).map(|function| Stray {
        at: Position::NONE,
        message: format!(
            "a script that uses closures can't define a function named `{}`: closures are only allowed as arguments \
             of the built-in function of that name",
            function.name
        ),
    })
}

/// Whether a constant is the function pointer of a closure.
fn is_closure(value: &Dynamic) -> bool {
    value.read_lock::<FnPtr>().is_some_and(|pointer| pointer.fn_name().starts_with("anon$"))
}

/// Whether the closure whose ancestors are `path` (outermost first) is an argument of a [`HOLDERS`] call. A closure
/// that captures a variable is wrapped by Rhai: `curry(closure, captured..)` inside a block that shares the captured
/// variables, and that block is the argument. Statements that only hold an expression are not counted as ancestors.
fn held_by_a_call(path: &[ASTNode<'_>]) -> bool {
    let path: Vec<&ASTNode<'_>> = path.iter().filter(|node| !matches!(node, ASTNode::Stmt(Stmt::Expr(_)))).collect();
    let is_curry = |node: &ASTNode<'_>| matches!(node, ASTNode::Expr(Expr::FnCall(call, _)) | ASTNode::Stmt(Stmt::FnCall(call, _)) if call.name == CURRY);
    let owner = match path.last() {
        Some(last) if is_curry(last) => match path.len().checked_sub(3).and_then(|index| path.get(index..index + 2)) {
            Some([owner, ASTNode::Expr(Expr::Stmt(_))]) => owner,
            _ => return false,
        },
        Some(owner) => owner,
        None => return false,
    };
    match owner {
        ASTNode::Expr(Expr::FnCall(call, _) | Expr::MethodCall(call, _)) | ASTNode::Stmt(Stmt::FnCall(call, _)) => {
            HOLDERS.contains(&call.name.as_str())
        }
        _ => false,
    }
}
