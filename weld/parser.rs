//! Top-down recursive descent parser for Weld.
//!
//! Weld is designed to be parseable in one left-to-right pass through the input, without
//! backtracking, so we simply track a position as we go and keep incrementing it.

use std::vec::Vec;
use std::cmp::min;

use super::ast::Symbol;
use super::ast::Iter;
use super::ast::BinOpKind::*;
use super::ast::UnaryOpKind;
use super::ast::UnaryOpKind::*;
use super::ast::ExprKind::*;
use super::ast::LiteralKind::*;
use super::ast::ScalarKind;
use super::ast::IterKind::*;
use super::colors::*;
use super::error::*;
use super::partial_types::*;
use super::partial_types::PartialBuilderKind::*;
use super::partial_types::PartialType::*;
use super::program::*;
use super::tokenizer::*;
use super::tokenizer::Token::*;

use super::annotations::*;

use std::error::Error;

#[cfg(test)]
use super::pretty_print::*;

/// Returns a formatted parse error if the parse failed, or returns the `res`.
macro_rules! check_parse_error {
    ($parser:expr, $res:expr) => ({
        if $res.is_ok() && !$parser.is_done() {
            return weld_err!("Unexpected token {} at {}", $parser.peek(), $parser.error_context());
        } else if $res.is_err() {
            return weld_err!("{} (at {})", $res.unwrap_err().description(), $parser.error_context());
        } else {
            $res
        }
    })
}

/// Parse the complete input string as a Weld program (optional macros plus one expression).
pub fn parse_program(input: &str) -> WeldResult<Program> {
    let tokens = try!(tokenize(input));
    let mut parser = Parser::new(&tokens);
    let res = parser.program();

    check_parse_error!(parser, res)
}

/// Parse the complete input string as a list of macros.
pub fn parse_macros(input: &str) -> WeldResult<Vec<Macro>> {
    let tokens = try!(tokenize(input));
    let mut parser = Parser::new(&tokens);
    let res = parser.macros();

    check_parse_error!(parser, res)
}

/// Parse the complete input string as an expression.
pub fn parse_expr(input: &str) -> WeldResult<PartialExpr> {
    let tokens = try!(tokenize(input));
    let mut parser = Parser::new(&tokens);
    let res = parser.expr().map(|b| *b);

    check_parse_error!(parser, res)
}

/// Parse the complete input string as a PartialType.
pub fn parse_type(input: &str) -> WeldResult<PartialType> {
    let tokens = try!(tokenize(input));
    let mut parser = Parser::new(&tokens);
    let res = parser.type_();

    check_parse_error!(parser, res)
}

/// A stateful object that parses a sequence of tokens, tracking its position at each point.
/// Assumes that the tokens end with a TEndOfInput.
struct Parser<'t> {
    tokens: &'t [Token],
    position: usize,
}

impl<'t> Parser<'t> {
    fn new(tokens: &[Token]) -> Parser {
        Parser {
            tokens: tokens,
            position: 0,
        }
    }

    /// Look at the next token to be parsed.
    fn peek(&self) -> &'t Token {
        &self.tokens[self.position]
    }

    /// Returns a string representing a context in the input.
    fn error_context(&self) -> String {
        let length = 10;
        let mut string = String::from("");
        let context_length = if self.position >= length {
            string.push_str("...");
            length
        } else {
            self.position
        };

        for i in (self.position - context_length)..min((self.position + context_length), self.tokens.len()-1) {
            let token_str = format!("{}", &self.tokens[i]);
            if i == self.position { 
                string.push_str(format_color(Color::BoldRed, token_str.as_str()).as_str());
            } else {
                string.push_str(format!("{}", token_str.as_str()).as_str());
            }

            if i != self.position - 1 && self.tokens[i+1].requires_space() {
                if self.tokens[i].requires_space() {
                    string.push_str(" ");
                }
            }
        }

        if self.position != self.tokens.len() {
            string.push_str("...");
        }

        string
    }

    /// Consume and return the next token.
    fn next(&mut self) -> &'t Token {
        let token = &self.tokens[self.position];
        self.position += 1;
        token
    }

    /// Consume the next token and check that it equals `expected`. If not, return an Err.
    fn consume(&mut self, expected: Token) -> WeldResult<()> {
        if *self.next() != expected {
            weld_err!("Expected '{}'", expected)
        } else {
            Ok(())
        }
    }

    /// Are we done parsing all the input?
    fn is_done(&self) -> bool {
        self.position == self.tokens.len() || *self.peek() == TEndOfInput
    }

    /// Parse a program (optional macros + one body expression) starting at the current position.
    fn program(&mut self) -> WeldResult<Program> {
        let macros = try!(self.macros());
        let body = try!(self.expr());
        Ok(Program {
               macros: macros,
               body: *body,
           })
    }

    /// Parse a list of macros starting at the current position.
    fn macros(&mut self) -> WeldResult<Vec<Macro>> {
        let mut res: Vec<Macro> = Vec::new();
        while *self.peek() == TMacro {
            res.push(try!(self.macro_()));
        }
        Ok(res)
    }

    /// Parse a single macro starting at the current position.
    fn macro_(&mut self) -> WeldResult<Macro> {
        try!(self.consume(TMacro));
        let name = try!(self.symbol());
        let mut params: Vec<Symbol> = Vec::new();
        try!(self.consume(TOpenParen));
        while *self.peek() != TCloseParen {
            params.push(try!(self.symbol()));
            if *self.peek() == TComma {
                self.next();
            } else if *self.peek() != TCloseParen {
                return weld_err!("Expected ',' or ')'");
            }
        }
        try!(self.consume(TCloseParen));
        try!(self.consume(TEqual));
        let body = try!(self.expr());
        try!(self.consume(TSemicolon));
        Ok(Macro {
               name: name,
               parameters: params,
               body: *body,
           })
    }

    /// Parse an expression starting at the current position.
    fn expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        if *self.peek() == TLet {
            self.let_expr()
        } else if *self.peek() == TBar || *self.peek() == TLogicalOr {
            self.lambda_expr()
        } else {
            self.operator_expr()
        }
    }

    /// Parse 'let name = value; body' starting at the current position.
    fn let_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        try!(self.consume(TLet));
        let name = try!(self.symbol());
        let ty = try!(self.optional_type());
        try!(self.consume(TEqual));
        let mut value = try!(self.operator_expr());

        // If a type was found, assign it (even if the value already has a known type).
        // Type inference will catch any type mismatches later on.
        if ty != Unknown {
            value.ty = ty;
        }

        try!(self.consume(TSemicolon));
        let body = try!(self.expr());
        let expr = expr_box(Let {
                                    name: name,
                                    value: value,
                                    body: body,
                                },
                                Annotations::new());
        Ok(expr)
    }

    /// Parse '|params| body' starting at the current position.
    fn lambda_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut params: Vec<PartialParameter> = Vec::new();
        // The next token could be either '||' if there are no params, or '|' if there are some.
        let token = self.next();
        if *token == TBar {
            while *self.peek() != TBar {
                let name = try!(self.symbol());
                let ty = try!(self.optional_type());
                params.push(PartialParameter { name: name, ty: ty });
                if *self.peek() == TComma {
                    self.next();
                } else if *self.peek() != TBar {
                    return weld_err!("Expected ',' or '|'");
                }
            }
            try!(self.consume(TBar));
        } else if *token != TLogicalOr {
            return weld_err!("Expected '|' or '||'");
        }
        let body = try!(self.expr());
        Ok(expr_box(Lambda {
                        params: params,
                        body: body,
                    },
                    Annotations::new()))
    }

    /// Parse an expression involving operators (||, &&, +, -, etc down the precedence chain)
    fn operator_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        self.logical_or_expr()
    }

    /// Parse a logical or expression with terms separated by || (for operator precedence).
    fn logical_or_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut res = try!(self.logical_and_expr());
        while *self.peek() == TLogicalOr {
            self.consume(TLogicalOr)?;
            let right = try!(self.logical_and_expr());
            res = expr_box(BinOp {
                               kind: LogicalOr,
                               left: res,
                               right: right,
                           },
                           Annotations::new())
        }
        Ok(res)
    }

    /// Parse a logical and expression with terms separated by && (for operator precedence).
    fn logical_and_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut res = try!(self.bitwise_or_expr());
        while *self.peek() == TLogicalAnd {
            self.consume(TLogicalAnd)?;
            let right = try!(self.bitwise_or_expr());
            res = expr_box(BinOp {
                               kind: LogicalAnd,
                               left: res,
                               right: right,
                           },
                           Annotations::new())
        }
        Ok(res)
    }

    /// Parse a bitwise or expression with terms separated by | (for operator precedence).
    fn bitwise_or_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut res = try!(self.xor_expr());
        while *self.peek() == TBar {
            self.consume(TBar)?;
            let right = try!(self.xor_expr());
            res = expr_box(BinOp {
                               kind: BitwiseOr,
                               left: res,
                               right: right,
                           },
                           Annotations::new())
        }
        Ok(res)
    }

    /// Parse a bitwise or expression with terms separated by ^ (for operator precedence).
    fn xor_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut res = try!(self.bitwise_and_expr());
        while *self.peek() == TXor {
            self.consume(TXor)?;
            let right = try!(self.bitwise_and_expr());
            res = expr_box(BinOp {
                               kind: Xor,
                               left: res,
                               right: right,
                           },
                           Annotations::new())
        }
        Ok(res)
    }

    /// Parse a bitwise and expression with terms separated by & (for operator precedence).
    fn bitwise_and_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut res = try!(self.equality_expr());
        while *self.peek() == TBitwiseAnd {
            self.consume(TBitwiseAnd)?;
            let right = try!(self.equality_expr());
            res = expr_box(BinOp {
                               kind: BitwiseAnd,
                               left: res,
                               right: right,
                           },
                           Annotations::new())
        }
        Ok(res)
    }

    /// Parse an == or != expression (for operator precedence).
    fn equality_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut res = try!(self.comparison_expr());
        // Unlike other expressions, we only allow one operator here; prevents stuff like a==b==c
        if *self.peek() == TEqualEqual || *self.peek() == TNotEqual {
            let token = self.next();
            let right = try!(self.comparison_expr());
            if *token == TEqualEqual {
                res = expr_box(BinOp {
                                   kind: Equal,
                                   left: res,
                                   right: right,
                               },
                               Annotations::new())
            } else {
                res = expr_box(BinOp {
                                   kind: NotEqual,
                                   left: res,
                                   right: right,
                               },
                               Annotations::new())
            }
        }
        Ok(res)
    }

    /// Parse a <, >, <= or >= expression (for operator precedence).
    fn comparison_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut res = try!(self.sum_expr());
        // Unlike other expressions, we only allow one operator here; prevents stuff like a>b>c
        if *self.peek() == TLessThan || *self.peek() == TLessThanOrEqual ||
           *self.peek() == TGreaterThan || *self.peek() == TGreaterThanOrEqual {
            let op = match *self.next() {
                TLessThan => LessThan,
                TGreaterThan => GreaterThan,
                TLessThanOrEqual => LessThanOrEqual,
                _ => GreaterThanOrEqual,
            };
            let right = try!(self.sum_expr());
            res = expr_box(BinOp {
                               kind: op,
                               left: res,
                               right: right,
                           },
                           Annotations::new())
        }
        Ok(res)
    }

    /// Parse a sum expression with terms separated by + and - (for operator precedence).
    fn sum_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut res = try!(self.product_expr());
        while *self.peek() == TPlus || *self.peek() == TMinus {
            let token = self.next();
            let right = try!(self.product_expr());
            if *token == TPlus {
                res = expr_box(BinOp {
                                   kind: Add,
                                   left: res,
                                   right: right,
                               },
                               Annotations::new())
            } else {
                res = expr_box(BinOp {
                                   kind: Subtract,
                                   left: res,
                                   right: right,
                               },
                               Annotations::new())
            }
        }
        Ok(res)
    }

    /// Parse a product expression with terms separated by *, / and % (for precedence).
    fn product_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut res = try!(self.ascribe_expr());
        while *self.peek() == TTimes || *self.peek() == TDivide || *self.peek() == TModulo {
            let op = match *self.next() {
                TTimes => Multiply,
                TDivide => Divide,
                _ => Modulo,
            };
            let right = try!(self.ascribe_expr());
            res = expr_box(BinOp {
                               kind: op,
                               left: res,
                               right: right,
                           },
                           Annotations::new())
        }
        Ok(res)
    }

    /// Parse a type abscription expression such as 'e: T', or lower-level ones in precedence.
    fn ascribe_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut expr = try!(self.apply_expr());
        if *self.peek() == TColon {
            expr.ty = try!(self.optional_type());
        }
        Ok(expr)
    }

    /// Parse application chain expression such as a.0().3().
    fn apply_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut expr = try!(self.leaf_expr());
        while *self.peek() == TDot || *self.peek() == TOpenParen {
            if *self.next() == TDot {
                match *self.next() {
                    TIdent(ref value) => {
                        if value.starts_with("$") {
                            match u32::from_str_radix(&value[1..], 10) {
                                Ok(index) => {
                                    expr = expr_box(GetField {
                                                        expr: expr,
                                                        index: index,
                                                    },
                                                    Annotations::new())
                                }
                                _ => return weld_err!("Expected field index but got '{}'", value),
                            }
                        }
                    }

                    ref other => return weld_err!("Expected field index but got '{}'", other),
                }
            } else {
                // TOpenParen
                let mut params: Vec<PartialExpr> = Vec::new();
                while *self.peek() != TCloseParen {
                    let param = try!(self.expr());
                    params.push(*param);
                    if *self.peek() == TComma {
                        self.next();
                    } else if *self.peek() != TCloseParen {
                        return weld_err!("Expected ',' or ')'");
                    }
                }
                try!(self.consume(TCloseParen));
                expr = expr_box(Apply {
                                    func: expr,
                                    params: params,
                                },
                                Annotations::new())
            }
        }
        Ok(expr)
    }

    /// Parses a for loop iterator expression starting at the current position. This could also be
    /// a vector expression (i.e., without an explicit iter(..).
    fn parse_iter(&mut self) -> WeldResult<Iter<PartialType>> {
        let iter: Token = self.peek().clone();
        match iter {
            TScalarIter | TSimdIter | TFringeIter => {
                try!(self.consume(iter.clone()));
                try!(self.consume(TOpenParen));
                let data = try!(self.expr());
                let mut start = None;
                let mut end = None;
                let mut stride = None;
                if *self.peek() == TComma {
                    try!(self.consume(TComma));
                    start = Some(try!(self.expr()));
                    try!(self.consume(TComma));
                    end = Some(try!(self.expr()));
                    try!(self.consume(TComma));
                    stride = Some(try!(self.expr()));
                }
                let iter = Iter {
                    data: data,
                    start: start,
                    end: end,
                    stride: stride,
                    kind: match iter {
                        TSimdIter => SimdIter,
                        TFringeIter => FringeIter,
                        _ => ScalarIter,
                    },
                };
                try!(self.consume(TCloseParen));
                Ok(iter)
            },
            TRangeIter => {
                try!(self.consume(iter.clone()));
                try!(self.consume(TOpenParen));
                let start = try!(self.expr());
                try!(self.consume(TComma));
                let end = try!(self.expr());
                try!(self.consume(TComma));
                let stride = try!(self.expr());
                let mut dummy_data = expr_box(MakeVector { elems: vec![] }, Annotations::new());
                dummy_data.as_mut().ty = Vector(Box::new(Scalar(ScalarKind::I64)));
                let iter = Iter {
                    data: dummy_data,
                    start: Some(start),
                    end: Some(end),
                    stride: Some(stride),
                    kind: RangeIter,
                };
                try!(self.consume(TCloseParen));
                Ok(iter)
            },
            _ => {
                let data = try!(self.expr());
                let iter = Iter {
                    data: data,
                    start: None,
                    end: None,
                    stride: None,
                    kind: ScalarIter,
                };
                Ok(iter)
            }
        }
    }

    /// Parses a cast operation, the type being cast to is passed in as an argument.
    fn parse_cast(&mut self, kind: ScalarKind) -> WeldResult<Box<PartialExpr>> {
        if *self.next() != TOpenParen {
            return weld_err!("Expected '('");
        }
        let expr = try!(self.expr());
        if *self.next() != TCloseParen {
            return weld_err!("Expected ')'");
        }
        let cast_expr = expr_box(Cast {
                                     kind: kind,
                                     child_expr: expr,
                                 },
                                 Annotations::new());
        Ok(cast_expr)
    }

    /// Parses annotations in the format "@(<annotation name>: <annotation value>,...)".
    fn parse_annotations(&mut self, annotations: &mut Annotations) -> WeldResult<()> {
        if *self.peek() == TAtMark {
            self.consume(TAtMark)?;
            try!(self.consume(TOpenParen));
            while *self.peek() != TCloseParen {
                match *self.peek() {
                    TIdent(ref value) => {
                        match value.as_ref() {
                            "impl" => {
                                self.consume(TIdent("impl".to_string()))?;
                                try!(self.consume(TColon));
                                let implementation = match *self.next() {
                                    TIdent(ref inner_value) => {
                                        match inner_value.as_ref() {
                                            "global" => BuilderImplementationKind::Global,
                                            "local" => BuilderImplementationKind::Local,
                                            _ => return weld_err!("Invalid implementation type"),
                                        }
                                    }
                                    _ => return weld_err!("Invalid implementation type"),
                                };
                                annotations.set_builder_implementation(implementation);
                            }
                            "predicate" => {
                                self.consume(TIdent("predicate".to_string()))?;
                                try!(self.consume(TColon));
                                if let TBoolLiteral(l) = *self.next() {
                                    annotations.set_predicate(l);
                                } else {
                                    return weld_err!("Invalid predicate type (must be a bool)");
                                }
                            }
                            "vectorize" => {
                                self.consume(TIdent("vectorize".to_string()))?;
                                try!(self.consume(TColon));
                                if let TBoolLiteral(l) = *self.next() {
                                    annotations.set_vectorize(l);
                                } else {
                                    return weld_err!("Invalid vectorize type (must be a bool)");
                                }
                            }
                            "tile_size" => {
                                self.consume(TIdent("tile_size".to_string()))?;
                                try!(self.consume(TColon));
                                if let TI32Literal(l) = *self.next() {
                                    annotations.set_tile_size(l);
                                } else {
                                    return weld_err!("Invalid tile size (must be a i32)");
                                }
                            }
                            "grain_size" => {
                                self.consume(TIdent("grain_size".to_string()))?;
                                try!(self.consume(TColon));
                                if let TI32Literal(l) = *self.next() {
                                    annotations.set_grain_size(l);
                                } else {
                                    return weld_err!("Invalid tile size (must be a i32)");
                                }
                            }
                            "size" => {
                                self.consume(TIdent("size".to_string()))?;
                                try!(self.consume(TColon));
                                if let TI64Literal(l) = *self.next() {
                                    annotations.set_size(l);
                                } else {
                                    return weld_err!("Invalid vector size (must be a i64)");
                                }
                            }
                            "loopsize" => {
                                self.consume(TIdent("loopsize".to_string()))?;
                                try!(self.consume(TColon));
                                if let TI64Literal(l) = *self.next() {
                                    annotations.set_loopsize(l);
                                } else {
                                    return weld_err!("Invalid vector size (must be a i64)");
                                }
                            }
                            "selectivity" => {
                                self.consume(TIdent("selectivity".to_string()))?;
                                try!(self.consume(TColon));
                                if let TF32Literal(l) = *self.next() {
                                    annotations.set_branch_selectivity((l * 100000.0) as i32);
                                } else {
                                    return weld_err!("Invalid selectivity (must be a f32)");
                                }
                            }
                            "num_keys" => {
                                self.consume(TIdent("num_keys".to_string()))?;
                                try!(self.consume(TColon));
                                if let TI64Literal(l) = *self.next() {
                                    annotations.set_num_keys(l);
                                } else {
                                    return weld_err!("Invalid number of keys (must be a i64)");
                                }
                            }
                            _ => return weld_err!("Invalid annotation type"),
                        }
                    }
                    _ => return weld_err!("Invalid annotation type -- expected an identifier"),
                }

                if *self.peek() == TComma {
                    self.next();
                } else if *self.peek() != TCloseParen {
                    return weld_err!("Expected ',' or ')'");
                }
            }
            try!(self.consume(TCloseParen));
        }
        Ok(())
    }

    /// Helper function which returns the `UnaryOpKind` for a token.
    fn unary_op_kind_for_token(&self, token: Token) -> WeldResult<UnaryOpKind> {
        let kind = match token {
            TExp => Exp,
            TLog => Log,
            TSqrt => Sqrt,
            TErf => Erf,
            TSin => Sin,
            TCos => Cos,
            TTan => Tan,
            TASin => ASin,
            TACos => ACos,
            TATan => ATan,
            TSinh => Sinh,
            TCosh => Cosh,
            TTanh => Tanh,
            _ => {
                return weld_err!("Invalid token for UnaryOp");
            }
        };
        Ok(kind)
    }

    /// Helper function for leaf_expr as all functions with unary args follow same pattern.
    fn unary_leaf_expr(&mut self, token: Token) -> WeldResult<Box<PartialExpr>> {
        try!(self.consume(TOpenParen));
        let value = try!(self.expr());
        try!(self.consume(TCloseParen));
        let kind = self.unary_op_kind_for_token(token)?;
        Ok(expr_box(UnaryOp {
            kind: kind,
            value: value,
        }, Annotations::new()))
    }

    /// Parse a terminal expression at the bottom of the precedence chain.
    fn leaf_expr(&mut self) -> WeldResult<Box<PartialExpr>> {
        let mut annotations = Annotations::new();
        try!(self.parse_annotations(&mut annotations));

        match *self.next() {
            TI8Literal(v) => Ok(expr_box(Literal(I8Literal(v)), Annotations::new())),
            TI32Literal(v) => Ok(expr_box(Literal(I32Literal(v)), Annotations::new())),
            TI64Literal(v) => Ok(expr_box(Literal(I64Literal(v)), Annotations::new())),
            TF32Literal(v) => Ok(expr_box(Literal(F32Literal(v.to_bits())), Annotations::new())),
            TF64Literal(v) => Ok(expr_box(Literal(F64Literal(v.to_bits())), Annotations::new())),
            TBoolLiteral(v) => Ok(expr_box(Literal(BoolLiteral(v)), Annotations::new())),
            TStringLiteral(ref v) => Ok(expr_box(Literal(StringLiteral(v.clone())),
                                                 Annotations::new())),

            TI8 => Ok(self.parse_cast(ScalarKind::I8)?),
            TI16 => Ok(self.parse_cast(ScalarKind::I16)?),
            TI32 => Ok(self.parse_cast(ScalarKind::I32)?),
            TI64 => Ok(self.parse_cast(ScalarKind::I64)?),
            TU8 => Ok(self.parse_cast(ScalarKind::U8)?),
            TU16 => Ok(self.parse_cast(ScalarKind::U16)?),
            TU32 => Ok(self.parse_cast(ScalarKind::U32)?),
            TU64 => Ok(self.parse_cast(ScalarKind::U64)?),
            TF32 => Ok(self.parse_cast(ScalarKind::F32)?),
            TF64 => Ok(self.parse_cast(ScalarKind::F64)?),
            TBool => Ok(self.parse_cast(ScalarKind::Bool)?),

            TToVec => {
                try!(self.consume(TOpenParen));
                let child_expr = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(ToVec { child_expr: child_expr }, Annotations::new()))
            }

            TIdent(ref name) => {
                Ok(expr_box(Ident(Symbol {
                                      name: name.clone(),
                                      id: 0,
                                  }),
                            Annotations::new()))
            }

            TOpenParen => {
                let expr = try!(self.expr());
                if *self.next() != TCloseParen {
                    return weld_err!("Expected ')' after {:?}", expr);
                }
                Ok(expr)
            }

            TOpenBracket => {
                let mut exprs: Vec<PartialExpr> = Vec::new();
                while *self.peek() != TCloseBracket {
                    let expr = try!(self.expr());
                    exprs.push(*expr);
                    if *self.peek() == TComma {
                        self.next();
                    } else if *self.peek() != TCloseBracket {
                        return weld_err!("Expected ',' or ']'");
                    }
                }
                try!(self.consume(TCloseBracket));
                Ok(expr_box(MakeVector { elems: exprs }, Annotations::new()))
            }

            TOpenBrace => {
                let mut exprs: Vec<PartialExpr> = Vec::new();
                while *self.peek() != TCloseBrace {
                    let expr = try!(self.expr());
                    exprs.push(*expr);
                    if *self.peek() == TComma {
                        self.next();
                    } else if *self.peek() != TCloseBrace {
                        return weld_err!("Expected ',' or '}}'");
                    }
                }
                try!(self.consume(TCloseBrace));
                Ok(expr_box(MakeStruct { elems: exprs }, Annotations::new()))
            }

            TIf => {
                try!(self.consume(TOpenParen));
                let cond = try!(self.expr());
                try!(self.consume(TComma));
                let on_true = try!(self.expr());
                try!(self.consume(TComma));
                let on_false = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(If {
                                cond: cond,
                                on_true: on_true,
                                on_false: on_false,
                            },
                            annotations))
            }

            TIterate => {
                try!(self.consume(TOpenParen));
                let initial = try!(self.expr());
                try!(self.consume(TComma));
                let update_func = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(Iterate {
                                initial: initial,
                                update_func: update_func,
                            },
                            Annotations::new()))
            }

            TSelect => {
                try!(self.consume(TOpenParen));
                let cond = try!(self.expr());
                try!(self.consume(TComma));
                let on_true = try!(self.expr());
                try!(self.consume(TComma));
                let on_false = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(Select {
                                cond: cond,
                                on_true: on_true,
                                on_false: on_false,
                            },
                            Annotations::new()))
            }

            TBroadcast => {
                try!(self.consume(TOpenParen));
                let expr = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(Broadcast(expr), Annotations::new()))
            }

            TCUDF => {
                let mut args = vec![];
                try!(self.consume(TOpenBracket));
                let sym_name = try!(self.symbol());
                try!(self.consume(TComma));
                let return_ty = try!(self.type_());
                try!(self.consume(TCloseBracket));
                try!(self.consume(TOpenParen));
                while *self.peek() != TCloseParen {
                    let arg = try!(self.expr());
                    args.push(*arg);
                    if *self.peek() == TComma {
                        try!(self.consume(TComma));
                    }
                }
                try!(self.consume(TCloseParen));
                Ok(expr_box(CUDF {
                                sym_name: sym_name.name,
                                return_ty: Box::new(return_ty),
                                args: args,
                            },
                            annotations))
            }

            TZip => {
                try!(self.consume(TOpenParen));
                let mut vectors = vec![];
                while *self.peek() != TCloseParen {
                    let vector = try!(self.expr());
                    vectors.push(*vector);
                    if *self.peek() == TComma {
                        self.next();
                    } else if *self.peek() != TCloseParen {
                        return weld_err!("Expected ',' or ')'");
                    }
                }
                try!(self.consume(TCloseParen));
                if vectors.len() < 2 {
                    return weld_err!("Expected two or more arguments in Zip");
                }
                Ok(expr_box(Zip { vectors: vectors }, Annotations::new()))
            }

            TFor => {
                try!(self.consume(TOpenParen));
                let mut iters = vec![];
                // Zips only appear as syntactic sugar in the context of Fors (they don't
                // become Zip expressions).
                if *self.peek() == TZip {
                    try!(self.consume(TZip));
                    try!(self.consume(TOpenParen));
                    iters.push(try!(self.parse_iter()));
                    while *self.peek() == TComma {
                        try!(self.consume(TComma));
                        iters.push(try!(self.parse_iter()));
                    }
                    try!(self.consume(TCloseParen));
                } else {
                    // Single unzipped vector.
                    iters.push(try!(self.parse_iter()));
                }
                try!(self.consume(TComma));
                let builders = try!(self.expr());
                try!(self.consume(TComma));
                let body = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(For {
                                iters: iters,
                                builder: builders,
                                func: body,
                            },
                            annotations))
            }

            TLen => {
                try!(self.consume(TOpenParen));
                let data = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(Length { data: data }, Annotations::new()))
            }

            TLookup => {
                try!(self.consume(TOpenParen));
                let data = try!(self.expr());
                try!(self.consume(TComma));
                let index = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(Lookup {
                                data: data,
                                index: index,
                            },
                            Annotations::new()))
            }

            TKeyExists => {
                try!(self.consume(TOpenParen));
                let data = try!(self.expr());
                try!(self.consume(TComma));
                let key = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(KeyExists {
                                data: data,
                                key: key,
                            },
                            Annotations::new()))
            }

            TSlice => {
                try!(self.consume(TOpenParen));
                let data = try!(self.expr());
                try!(self.consume(TComma));
                let index = try!(self.expr());
                try!(self.consume(TComma));
                let size = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(Slice {
                                data: data,
                                index: index,
                                size: size,
                            },
                            Annotations::new()))
            }

            TSort => {
                try!(self.consume(TOpenParen));
                let data = try!(self.expr());
                try!(self.consume(TComma));
                let keyfunc = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(Sort {
                                data: data,
                                keyfunc: keyfunc,
                            },
                            Annotations::new()))
            }

            TExp => self.unary_leaf_expr(TExp),
            TLog => self.unary_leaf_expr(TLog),
            TErf => self.unary_leaf_expr(TErf),
            TSqrt => self.unary_leaf_expr(TSqrt),
            TSin => self.unary_leaf_expr(TSin),
            TCos => self.unary_leaf_expr(TCos),
            TTan => self.unary_leaf_expr(TTan),
            TASin => self.unary_leaf_expr(TASin),
            TACos => self.unary_leaf_expr(TACos),
            TATan => self.unary_leaf_expr(TATan),
            TSinh => self.unary_leaf_expr(TSinh),
            TCosh => self.unary_leaf_expr(TCosh),
            TTanh => self.unary_leaf_expr(TTanh),

            TMerge => {
                try!(self.consume(TOpenParen));
                let builder = try!(self.expr());
                try!(self.consume(TComma));
                let value = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(Merge {
                                builder: builder,
                                value: value,
                            },
                            Annotations::new()))
            }

            TResult => {
                try!(self.consume(TOpenParen));
                let value = try!(self.expr());
                try!(self.consume(TCloseParen));
                Ok(expr_box(Res { builder: value }, Annotations::new()))
            }

            TAppender => {
                let mut elem_type = Unknown;
                if *self.peek() == TOpenBracket {
                    try!(self.consume(TOpenBracket));
                    elem_type = try!(self.type_());
                    try!(self.consume(TCloseBracket));
                }

                let arg = if *self.peek() == TOpenParen {
                    self.consume(TOpenParen)?;
                    let arg = self.expr()?;
                    self.consume(TCloseParen)?;
                    Some(arg)
                } else {
                    None
                };

                let mut expr = expr_box(NewBuilder(arg), Annotations::new());
                expr.ty = Builder(Appender(Box::new(elem_type)), annotations);
                Ok(expr)
            }

            TMerger => {
                let elem_type: PartialType;
                let bin_op: _;
                self.consume(TOpenBracket)?;
                elem_type = self.type_()?;
                self.consume(TComma)?;
                // Basic merger supports Plus and Times right now.
                match *self.peek() {
                    TPlus => {
                        self.consume(TPlus)?;
                        bin_op = Add;
                    }
                    TTimes => {
                        self.consume(TTimes)?;
                        bin_op = Multiply;
                    }
                    ref t => {
                        return weld_err!("expected commutative binary op in merger but got '{}'",
                                         t);
                    }
                };
                self.consume(TCloseBracket)?;

                let mut value = None;
                if *self.peek() == TOpenParen {
                    self.consume(TOpenParen)?;
                    value = Some(self.expr()?);
                    self.consume(TCloseParen)?;
                }

                let mut expr = expr_box(NewBuilder(value), Annotations::new());
                expr.ty = Builder(Merger(Box::new(elem_type), bin_op), annotations);
                Ok(expr)
            }

            TDictMerger => {
                let key_type: PartialType;
                let value_type: PartialType;
                let bin_op: _;
                try!(self.consume(TOpenBracket));
                key_type = try!(self.type_());
                try!(self.consume(TComma));
                value_type = try!(self.type_());
                try!(self.consume(TComma));
                // DictMerger right now supports Plus and Times only.
                match *self.peek() {
                    TPlus => {
                        self.consume(TPlus)?;
                        bin_op = Add;
                    }
                    TTimes => {
                        self.consume(TTimes)?;
                        bin_op = Multiply;
                    }
                    _ => {
                        return weld_err!("expected commutative binary op in dictmerger");
                    }
                }
                try!(self.consume(TCloseBracket));

                let arg = if *self.peek() == TOpenParen {
                    self.consume(TOpenParen)?;
                    let arg = self.expr()?;
                    self.consume(TCloseParen)?;
                    Some(arg)
                } else {
                    None
                };

                let mut expr = expr_box(NewBuilder(arg), Annotations::new());
                expr.ty = Builder(DictMerger(Box::new(key_type.clone()),
                                             Box::new(value_type.clone()),
                                             Box::new(Struct(vec![key_type.clone(),
                                                                  value_type.clone()])),
                                             bin_op),
                                  annotations);
                Ok(expr)
            }

            TGroupMerger => {
                let key_type: PartialType;
                let value_type: PartialType;
                try!(self.consume(TOpenBracket));
                key_type = try!(self.type_());
                try!(self.consume(TComma));
                value_type = try!(self.type_());
                try!(self.consume(TCloseBracket));
                let mut expr = expr_box(NewBuilder(None), Annotations::new());
                expr.ty = Builder(GroupMerger(Box::new(key_type.clone()),
                                              Box::new(value_type.clone()),
                                              Box::new(Struct(vec![key_type.clone(),
                                                                   value_type.clone()]))),
                                  annotations);
                Ok(expr)
            }

            TVecMerger => {
                let elem_type: PartialType;
                let bin_op: _;
                try!(self.consume(TOpenBracket));
                elem_type = try!(self.type_());
                try!(self.consume(TComma));
                // VecMerger right now supports Plus and Times only.
                match *self.peek() {
                    TPlus => {
                        self.consume(TPlus)?;
                        bin_op = Add;
                    }
                    TTimes => {
                        self.consume(TTimes)?;
                        bin_op = Multiply;
                    }
                    _ => {
                        return weld_err!("Expected commutative binary op in vecmerger");
                    }
                }
                try!(self.consume(TCloseBracket));
                try!(self.consume(TOpenParen));
                let expr = try!(self.expr());
                try!(self.consume(TCloseParen));

                let mut expr = expr_box(NewBuilder(Some(expr)), Annotations::new());
                expr.ty = Builder(VecMerger(Box::new(elem_type.clone()),
                                            Box::new(Struct(vec![Scalar(ScalarKind::I64),
                                                                 elem_type.clone()])),
                                            bin_op),
                                  annotations);
                Ok(expr)
            }

            TMinus => Ok(expr_box(Negate(try!(self.leaf_expr())), Annotations::new())),

            TMin => {
                try!(self.consume(TOpenParen));
                let left = try!(self.expr());
                try!(self.consume(TComma));
                let right = try!(self.expr());
                try!(self.consume(TCloseParen));
                
                let res = expr_box(BinOp {
                    kind: Min,
                    left: left,
                    right: right,
                }, Annotations::new());
                
                Ok(res)
            }

            TMax => {
                try!(self.consume(TOpenParen));
                let left = try!(self.expr());
                try!(self.consume(TComma));
                let right = try!(self.expr());
                try!(self.consume(TCloseParen));
                
                let res = expr_box(BinOp {
                    kind: Max,
                    left: left,
                    right: right,
                }, Annotations::new());
                
                Ok(res)
            }

            TPow => {
                try!(self.consume(TOpenParen));
                let left = try!(self.expr());
                try!(self.consume(TComma));
                let right = try!(self.expr());
                try!(self.consume(TCloseParen));
                
                let res = expr_box(BinOp {
                    kind: Pow,
                    left: left,
                    right: right,
                }, Annotations::new());
                Ok(res)
            }
            
            ref other => weld_err!("Expected expression but got '{}'", other),
        }
    }

    /// Parse a symbol starting at the current input position.
    fn symbol(&mut self) -> WeldResult<Symbol> {
        match *self.next() {
            TIdent(ref name) => {
                Ok(Symbol {
                       name: name.clone(),
                       id: 0,
                   })
            }
            ref other => weld_err!("Expected identifier but got '{}'", other),
        }
    }

    /// Optionally parse a type annotation such as ": i32" and return the result as a PartialType;
    /// gives Unknown if there is no type annotation at the current position.
    fn optional_type(&mut self) -> WeldResult<PartialType> {
        if *self.peek() == TColon {
            try!(self.consume(TColon));
            self.type_()
        } else {
            Ok(Unknown)
        }
    }

    /// Parse a PartialType starting at the current input position.
    fn type_(&mut self) -> WeldResult<PartialType> {
        let mut annotations = Annotations::new();
        try!(self.parse_annotations(&mut annotations));

        match *self.next() {
            TI8 => Ok(Scalar(ScalarKind::I8)),
            TI16 => Ok(Scalar(ScalarKind::I16)),
            TI32 => Ok(Scalar(ScalarKind::I32)),
            TI64 => Ok(Scalar(ScalarKind::I64)),
            TU8 => Ok(Scalar(ScalarKind::U8)),
            TU16 => Ok(Scalar(ScalarKind::U16)),
            TU32 => Ok(Scalar(ScalarKind::U32)),
            TU64 => Ok(Scalar(ScalarKind::U64)),
            TF32 => Ok(Scalar(ScalarKind::F32)),
            TF64 => Ok(Scalar(ScalarKind::F64)),
            TBool => Ok(Scalar(ScalarKind::Bool)),

            TVec => {
                try!(self.consume(TOpenBracket));
                let elem_type = try!(self.type_());
                try!(self.consume(TCloseBracket));
                Ok(Vector(Box::new(elem_type)))
            }

            TSimd => {
                try!(self.consume(TOpenBracket));
                let elem_type = try!(self.type_());
                try!(self.consume(TCloseBracket));
                if let Scalar(ref kind) = elem_type {
                    Ok(Simd(kind.clone()))
                } else {
                    weld_err!("Expected Scalar type in simd")
                }
            }

            TAppender => {
                try!(self.consume(TOpenBracket));
                let elem_type = try!(self.type_());
                try!(self.consume(TCloseBracket));


                Ok(Builder(Appender(Box::new(elem_type)), annotations))
            }

            TMerger => {
                let elem_type: PartialType;
                let bin_op: _;
                self.consume(TOpenBracket)?;
                elem_type = self.type_()?;
                self.consume(TComma)?;
                // Basic merger supports Plus and Times right now.
                match *self.peek() {
                    TPlus => {
                        self.consume(TPlus)?;
                        bin_op = Add;
                    }
                    TTimes => {
                        self.consume(TTimes)?;
                        bin_op = Multiply;
                    }
                    ref t => {
                        return weld_err!("expected commutative binary op in merger but got '{}'",
                                         t);
                    }
                };
                self.consume(TCloseBracket)?;

                Ok(Builder(Merger(Box::new(elem_type), bin_op), annotations))
            }

            TDictMerger => {
                let key_type: PartialType;
                let value_type: PartialType;
                let bin_op: _;
                try!(self.consume(TOpenBracket));
                key_type = try!(self.type_());
                try!(self.consume(TComma));
                value_type = try!(self.type_());
                try!(self.consume(TComma));
                // DictMerger right now supports Plus and Times only.
                match *self.peek() {
                    TPlus => {
                        self.consume(TPlus)?;
                        bin_op = Add;
                    }
                    TTimes => {
                        self.consume(TTimes)?;
                        bin_op = Multiply;
                    }
                    _ => {
                        return weld_err!("expected commutative binary op in dictmerger");
                    }
                }
                try!(self.consume(TCloseBracket));
                Ok(Builder(DictMerger(Box::new(key_type.clone()),
                                      Box::new(value_type.clone()),
                                      Box::new(Struct(vec![key_type.clone(),
                                                           value_type.clone()])),
                                      bin_op),
                           annotations))
            }

            TVecMerger => {
                let elem_type: PartialType;
                let bin_op: _;
                try!(self.consume(TOpenBracket));
                elem_type = try!(self.type_());
                try!(self.consume(TComma));
                // VecMerger right now supports Plus and Times only.
                match *self.peek() {
                    TPlus => {
                        self.consume(TPlus)?;
                        bin_op = Add;
                    }
                    TTimes => {
                        self.consume(TTimes)?;
                        bin_op = Multiply;
                    }
                    _ => {
                        return weld_err!("Expected commutative binary op in vecmerger");
                    }
                }
                try!(self.consume(TCloseBracket));

                Ok(Builder(VecMerger(Box::new(elem_type.clone()),
                                     Box::new(Struct(vec![Scalar(ScalarKind::I64),
                                                          elem_type.clone()])),
                                     bin_op),
                           annotations))
            }

            TOpenBrace => {
                let mut types: Vec<PartialType> = Vec::new();
                while *self.peek() != TCloseBrace {
                    let ty = try!(self.type_());
                    types.push(ty);
                    if *self.peek() == TComma {
                        self.next();
                    } else if *self.peek() != TCloseBrace {
                        return weld_err!("Expected ',' or '}}'");
                    }
                }
                try!(self.consume(TCloseBrace));
                Ok(Struct(types))
            }

            TQuestion => Ok(Unknown),

            ref other => weld_err!("Expected type but got '{}'", other),
        }
    }
}

#[test]
fn basic_parsing() {
    let e = parse_expr("10 - 2 - 3 + 1").unwrap();
    assert_eq!(print_expr_without_indent(&e), "(((10-2)-3)+1)");

    let e = parse_expr("10 * 2 - 4 - 3 / 1").unwrap();
    assert_eq!(print_expr_without_indent(&e), "(((10*2)-4)-(3/1))");

    let e = parse_expr("i32(10 + 3 + 2)").unwrap();
    assert_eq!(print_expr_without_indent(&e), "(i32(((10+3)+2)))");

    let e = parse_expr("10 + 64 + i32(10.0)").unwrap();
    assert_eq!(print_expr_without_indent(&e), "((10+64)+(i32(10.0)))");

    let e = parse_expr("10 + 64 + f32(bool(19))").unwrap();
    assert_eq!(print_expr_without_indent(&e), "((10+64)+(f32((bool(19)))))");

    let e = parse_expr("1L:i64 + i64(1)").unwrap();
    assert_eq!(print_expr_without_indent(&e), "(1L+(i64(1)))");

    let e = parse_expr("i64(1L:i64)").unwrap();
    assert_eq!(print_expr_without_indent(&e), "(i64(1L))");

    let e = parse_expr("[1, 2+3, 2]").unwrap();
    assert_eq!(print_expr_without_indent(&e), "[1,(2+3),2]");

    let e = parse_expr("let a = 3+2; let b = (let c=a; c); b").unwrap();
    assert_eq!(print_expr_without_indent(&e),
               "(let a=((3+2));(let b=((let c=(a);c));b))");

    let e = parse_expr("let a: vec[i32] = [2, 3]; a").unwrap();
    assert_eq!(print_expr_without_indent(&e), "(let a=([2,3]);a)");

    let e = parse_expr("|a, b:i32| a+b").unwrap();
    assert_eq!(print_typed_expr_without_indent(&e), "|a:?,b:i32|(a:?+b:?)");

    let e = parse_expr("|| a||b").unwrap();
    assert_eq!(print_expr_without_indent(&e), "||(a||b)");

    let e = parse_expr("a.$0.$1").unwrap();
    assert_eq!(print_expr_without_indent(&e), "a.$0.$1");

    let e = parse_expr("a(0,1).$0").unwrap();
    assert_eq!(print_expr_without_indent(&e), "(a)(0,1).$0");

    let e = parse_expr("a.$0(0,1).$1()").unwrap();
    assert_eq!(print_expr_without_indent(&e), "((a.$0)(0,1).$1)()");

    let e = parse_expr("a>b==c").unwrap();
    assert_eq!(print_expr_without_indent(&e), "((a>b)==c)");

    assert!(parse_expr("a>b>c").is_err());
    assert!(parse_expr("a==b==c").is_err());

    let e = parse_expr("appender[?]").unwrap();
    assert_eq!(print_expr_without_indent(&e), "appender[?]");

    let e = parse_expr("appender[i32]").unwrap();
    assert_eq!(print_expr_without_indent(&e), "appender[i32]");

    let e = parse_expr("appender[i32](1000L)").unwrap();
    assert_eq!(print_expr_without_indent(&e), "appender[i32](1000L)");

    let e = parse_expr("@(impl:local) dictmerger[i32,i32,+]").unwrap();
    assert_eq!(print_expr_without_indent(&e),
               "@(impl:local)dictmerger[i32,i32,+]");

    let e = parse_expr("@(impl:local, num_keys:12l) dictmerger[i32,i32,+]").unwrap();
    assert_eq!(print_expr_without_indent(&e),
               "@(impl:local,num_keys:12)dictmerger[i32,i32,+]");

    assert!(parse_expr("@(impl:local, num_keys:12) dictmerger[i32,i32,+]").is_err());

    let e = parse_expr("a: i32 + b").unwrap();
    assert_eq!(print_typed_expr_without_indent(&e), "(a:i32+b:?)");

    let e = parse_expr("|a:i8| a").unwrap();
    assert_eq!(print_typed_expr_without_indent(&e), "|a:i8|a:?");

    assert!(parse_expr("10 * * 2").is_err());

    let p = parse_program("macro a(x) = x+x; macro b() = 5; a(b)").unwrap();
    assert_eq!(p.macros.len(), 2);
    assert_eq!(print_expr_without_indent(&p.body), "(a)(b)");
    assert_eq!(print_expr_without_indent(&p.macros[0].body), "(x+x)");
    assert_eq!(print_expr_without_indent(&p.macros[1].body), "5");

    let t = parse_type("{i32, vec[vec[?]], ?}").unwrap();
    assert_eq!(print_type(&t), "{i32,vec[vec[?]],?}");

    let t = parse_type("{}").unwrap();
    assert_eq!(print_type(&t), "{}");
}

#[test]
fn operator_precedence() {
    let e = parse_expr("a - b - c - d").unwrap();
    assert_eq!(print_expr_without_indent(&e), "(((a-b)-c)-d)");

    let e = parse_expr("a || b && c | d ^ e & f == g > h + i * j").unwrap();
    assert_eq!(print_expr_without_indent(&e),
               "(a||(b&&(c|(d^(e&(f==(g>(h+(i*j)))))))))");

    let e = parse_expr("a * b + c > d == e & f ^ g | h && i || j").unwrap();
    assert_eq!(print_expr_without_indent(&e),
               "(((((((((a*b)+c)>d)==e)&f)^g)|h)&&i)||j)");

    let e = parse_expr("a / b - c <= d != e & f ^ g | h && i || j").unwrap();
    assert_eq!(print_expr_without_indent(&e),
               "(((((((((a/b)-c)<=d)!=e)&f)^g)|h)&&i)||j)");

    let e = parse_expr("a % b - c >= d != e & f ^ g | h && i || j").unwrap();
    assert_eq!(print_expr_without_indent(&e),
               "(((((((((a%b)-c)>=d)!=e)&f)^g)|h)&&i)||j)");
}

#[test]
fn read_to_end_of_input() {
    assert!(parse_expr("a + b").is_ok());
    assert!(parse_expr("a + b macro").is_err());
    assert!(parse_type("vec[i32]").is_ok());
    assert!(parse_expr("vec[i32] 1").is_err());
    assert!(parse_program("macro a() = b; a() + b").is_ok());
    assert!(parse_program("macro a() = b; a() + b;").is_err());
}
