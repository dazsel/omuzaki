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

/// The functions a closure may be passed to, each with the argument position the closure may take and the numbers of
/// arguments the call may have: `try_op(closure)`, and the array helpers of Rhai's `BasicArrayPackage`, which take the
/// array first (`array.map(closure)`) and call the function and drop it. Naming the position and the arity, not only the
/// function, keeps a closure out of any argument the callee might keep: `array.map(closure)` takes it as the function,
/// and no other form of the call does.
const HOLDERS: [(&str, usize, &[usize]); 14] = [
    ("try_op", 0, &[1]),
    ("for_each", 1, &[2]),
    ("map", 1, &[2]),
    ("filter", 1, &[2]),
    ("find", 1, &[2]),
    ("find_map", 1, &[2]),
    ("some", 1, &[2]),
    ("all", 1, &[2]),
    ("dedup", 1, &[2]),
    // `reduce(function)` and `reduce(function, initial)`.
    ("reduce", 1, &[2, 3]),
    ("reduce_rev", 1, &[2, 3]),
    ("sort", 1, &[2]),
    ("drain", 1, &[2]),
    ("retain", 1, &[2]),
];

/// The accepted callers' names.
fn holder_names() -> impl Iterator<Item = &'static str> {
    HOLDERS.iter().map(|(name, ..)| *name)
}

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
        let Some(ASTNode::Expr(closure @ Expr::DynamicConstant(value, at))) = nodes.last() else { return true };
        if !is_closure(value) || held_by_a_call(&nodes[..nodes.len() - 1], closure) {
            return true;
        }
        found = Some(Stray {
            at: *at,
            message: format!(
                "a closure can only be passed straight to `try_op` or to an array function ({}); it can't be kept in \
                 a variable, an array or a map, or returned",
                holder_names().skip(1).collect::<Vec<_>>().join(", ")
            ),
        });
        false
    });
    if found.is_some() {
        return found;
    }
    let shadowing = ast.iter_functions().find(|function| holder_names().any(|name| name == function.name));
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

/// Whether `closure`, whose ancestors are `path` (outermost first), is the argument a [`HOLDERS`] call takes a function
/// in, in a call of an arity it takes. A closure that captures a variable is wrapped by Rhai: `curry(closure,
/// captured..)` inside a block that shares the captured variables, and that block is the argument. Statements that only
/// hold an expression are not counted as ancestors.
fn held_by_a_call(path: &[ASTNode<'_>], closure: &Expr) -> bool {
    let path: Vec<&ASTNode<'_>> = path.iter().filter(|node| !matches!(node, ASTNode::Stmt(Stmt::Expr(_)))).collect();
    let is_curry = |node: &ASTNode<'_>| matches!(node, ASTNode::Expr(Expr::FnCall(call, _)) | ASTNode::Stmt(Stmt::FnCall(call, _)) if call.name == CURRY);
    let (owner, argument) = match path.as_slice() {
        [.., owner, ASTNode::Expr(wrapper @ Expr::Stmt(_)), curry] if is_curry(curry) => (*owner, *wrapper),
        [.., owner] => (*owner, closure),
        [] => return false,
    };
    let (call, receiver) = match owner {
        ASTNode::Expr(Expr::FnCall(call, _)) | ASTNode::Stmt(Stmt::FnCall(call, _)) => (call, 0),
        // In `array.map(f)` the syntax tree keeps the array apart from the arguments; it is the call's first argument.
        ASTNode::Expr(Expr::MethodCall(call, _)) => (call, 1),
        _ => return false,
    };
    let Some(index) = call.args.iter().position(|arg| std::ptr::eq(arg, argument)) else { return false };
    let (position, arity) = (index + receiver, call.args.len() + receiver);
    HOLDERS.iter().any(|(name, at, arities)| *name == call.name.as_str() && *at == position && arities.contains(&arity))
}
