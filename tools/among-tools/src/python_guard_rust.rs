use crate::python_guard::{add, interpreter, tokens, Finding};
use syn::{
    spanned::Spanned,
    visit::{self, Visit},
    Expr,
};

fn strings(expression: &Expr, output: &mut Vec<String>) {
    match expression {
        // `env!("CARGO_BIN_EXE_<name>")` is expanded by cargo at compile time to
        // the absolute path of the binary it just built from this workspace's
        // Rust sources. It is a resolved executable by construction: an
        // interpreter cannot occupy that path. Treating it as resolved closes a
        // review gap rather than opening one. Any other `env!` stays unresolved,
        // because it can name anything.
        Expr::Macro(macro_call) if macro_call.mac.path.is_ident("env") => {
            if let Ok(literal) = macro_call.mac.parse_body::<syn::LitStr>() {
                if literal.value().starts_with("CARGO_BIN_EXE_") {
                    output.push(literal.value());
                }
            }
        }
        Expr::Lit(literal) => {
            if let syn::Lit::Str(value) = &literal.lit {
                output.push(value.value());
            }
        }
        Expr::Array(array) => {
            for value in &array.elems {
                strings(value, output);
            }
        }
        Expr::Reference(reference) => strings(&reference.expr, output),
        Expr::Paren(paren) => strings(&paren.expr, output),
        Expr::Group(group) => strings(&group.expr, output),
        Expr::MethodCall(call)
            if call.args.is_empty()
                && matches!(
                    call.method.to_string().as_str(),
                    "to_owned" | "to_string" | "as_str" | "as_os_str"
                ) =>
        {
            strings(&call.receiver, output);
        }
        Expr::Call(call) if call.args.len() == 1 => {
            if let Expr::Path(path) = call.func.as_ref() {
                let names = path
                    .path
                    .segments
                    .iter()
                    .map(|part| part.ident.to_string())
                    .collect::<Vec<_>>();
                if ["Path", "PathBuf", "OsStr", "OsString", "String"]
                    .iter()
                    .any(|name| {
                        names.ends_with(&[(*name).into(), "new".into()])
                            || names.ends_with(&[(*name).into(), "from".into()])
                    })
                {
                    strings(&call.args[0], output);
                }
            }
        }
        Expr::Binary(binary) if matches!(binary.op, syn::BinOp::Add(_)) => {
            let mut left = Vec::new();
            let mut right = Vec::new();
            strings(&binary.left, &mut left);
            strings(&binary.right, &mut right);
            if left.len() == 1 && right.len() == 1 {
                output.push(format!("{}{}", left[0], right[0]));
            }
        }
        Expr::Macro(mac) if mac.mac.path.is_ident("concat") => {
            use syn::parse::Parser;
            if let Ok(values) =
                syn::punctuated::Punctuated::<syn::LitStr, syn::Token![,]>::parse_terminated
                    .parse2(mac.mac.tokens.clone())
            {
                output.push(values.iter().map(syn::LitStr::value).collect());
            }
        }
        _ => {}
    }
}
struct Commands<'a> {
    path: &'a str,
    findings: &'a mut Vec<Finding>,
}
impl Commands<'_> {
    fn arguments(
        &mut self,
        arguments: impl Iterator<Item = impl std::borrow::Borrow<Expr>>,
        line: usize,
    ) {
        let mut values = Vec::new();
        for argument in arguments {
            strings(argument.borrow(), &mut values);
        }
        if values
            .iter()
            .any(|value| interpreter(value) || tokens(value).any(interpreter))
        {
            add(
                self.findings,
                self.path,
                line,
                "Rust interpreter delegation",
            );
        }
        if values.iter().any(|value| {
            tokens(value).any(|word| word.ends_with(".py") && !matches!(word, ".py" | "*.py"))
        }) {
            add(
                self.findings,
                self.path,
                line,
                "Rust Python caller reference, including excluded caches",
            );
        }
    }
}
impl<'ast> Visit<'ast> for Commands<'_> {
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Expr::Path(path) = node.func.as_ref() {
            let names = path
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            if names.ends_with(&["Command".into(), "new".into()]) {
                let mut executable = Vec::new();
                for arg in &node.args {
                    strings(arg, &mut executable);
                }
                // unresolved execution is a review gap, never a Python-free
                // result; runtime input and variable aliasing need separate proof.
                if executable.is_empty() {
                    add(self.findings, self.path, node.span().start().line,
                        "unresolved Rust dynamic executable; cannot certify interpreter prohibition");
                }
                self.arguments(node.args.iter(), node.span().start().line);
            }
        }
        visit::visit_expr_call(self, node);
    }
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if matches!(node.method.to_string().as_str(), "arg" | "args") {
            self.arguments(node.args.iter(), node.span().start().line);
        }
        visit::visit_expr_method_call(self, node);
    }
}

pub(super) fn inspect(path: &str, text: &str, findings: &mut Vec<Finding>) {
    let mut visitor = Commands { path, findings };
    if let Ok(file) = syn::parse_file(text) {
        visitor.visit_file(&file);
    } else if let Ok(block) = syn::parse_str::<syn::Block>(&format!("{{{text}}}")) {
        visitor.visit_block(&block);
    } else {
        add(
            visitor.findings,
            path,
            1,
            "unparseable Rust input; cannot certify interpreter delegation scan",
        );
    }
}
