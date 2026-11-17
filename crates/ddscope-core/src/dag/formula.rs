//! Lightweight mathematical expression compiler and stack-bytecode evaluator for virtual channels.

use crate::types::{CoreError, CoreResult, TimestampNs};

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    ChannelRef(String),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Bang,
    EqEq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    AndAnd,
    OrOr,
    Question,
    Colon,
    LParen,
    RParen,
    Comma,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum UnaryOp {
    Neg,
    Not,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Eq,
    NotEq,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum MathFunc {
    Abs,
    Sqrt,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Hypot,
    Min,
    Max,
    Clamp,
    Ln,
    Log10,
    Exp,
    Floor,
    Ceil,
    Round,
    Sign,
    Deg,
    Rad,
}

impl MathFunc {
    fn from_name(name: &str) -> Option<(Self, usize)> {
        match name {
            "abs" => Some((Self::Abs, 1)),
            "sqrt" => Some((Self::Sqrt, 1)),
            "sin" => Some((Self::Sin, 1)),
            "cos" => Some((Self::Cos, 1)),
            "tan" => Some((Self::Tan, 1)),
            "asin" => Some((Self::Asin, 1)),
            "acos" => Some((Self::Acos, 1)),
            "atan" => Some((Self::Atan, 1)),
            "atan2" => Some((Self::Atan2, 2)),
            "hypot" => Some((Self::Hypot, 2)),
            "min" => Some((Self::Min, 2)),
            "max" => Some((Self::Max, 2)),
            "clamp" => Some((Self::Clamp, 3)),
            "ln" => Some((Self::Ln, 1)),
            "log10" => Some((Self::Log10, 1)),
            "exp" => Some((Self::Exp, 1)),
            "floor" => Some((Self::Floor, 1)),
            "ceil" => Some((Self::Ceil, 1)),
            "round" => Some((Self::Round, 1)),
            "sign" => Some((Self::Sign, 1)),
            "deg" => Some((Self::Deg, 1)),
            "rad" => Some((Self::Rad, 1)),
            _ => None,
        }
    }

    #[inline]
    fn eval_1(self, a: f64) -> f64 {
        match self {
            Self::Abs => a.abs(),
            Self::Sqrt => {
                if a < 0.0 {
                    0.0
                } else {
                    a.sqrt()
                }
            }
            Self::Sin => a.sin(),
            Self::Cos => a.cos(),
            Self::Tan => a.tan(),
            Self::Asin => a.clamp(-1.0, 1.0).asin(),
            Self::Acos => a.clamp(-1.0, 1.0).acos(),
            Self::Atan => a.atan(),
            Self::Ln => {
                if a <= 0.0 {
                    0.0
                } else {
                    a.ln()
                }
            }
            Self::Log10 => {
                if a <= 0.0 {
                    0.0
                } else {
                    a.log10()
                }
            }
            Self::Exp => a.exp(),
            Self::Floor => a.floor(),
            Self::Ceil => a.ceil(),
            Self::Round => a.round(),
            Self::Sign => {
                if a > 0.0 {
                    1.0
                } else if a < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            }
            Self::Deg => a.to_degrees(),
            Self::Rad => a.to_radians(),
            Self::Atan2 | Self::Hypot | Self::Min | Self::Max | Self::Clamp => 0.0,
        }
    }

    #[inline]
    fn eval_2(self, a: f64, b: f64) -> f64 {
        match self {
            Self::Atan2 => a.atan2(b),
            Self::Hypot => a.hypot(b),
            Self::Min => a.min(b),
            Self::Max => a.max(b),
            _ => 0.0,
        }
    }

    #[inline]
    fn eval_3(self, a: f64, b: f64, c: f64) -> f64 {
        match self {
            Self::Clamp => {
                let (lo, hi) = if b <= c { (b, c) } else { (c, b) };
                a.clamp(lo, hi)
            }
            _ => 0.0,
        }
    }
}

#[derive(Clone, Debug)]
enum Ast {
    Const(f64),
    Channel(usize),
    TimeSec,
    Unary(UnaryOp, Box<Ast>),
    Binary(BinaryOp, Box<Ast>, Box<Ast>),
    LogicalAnd(Box<Ast>, Box<Ast>),
    LogicalOr(Box<Ast>, Box<Ast>),
    Ternary(Box<Ast>, Box<Ast>, Box<Ast>),
    Call1(MathFunc, Box<Ast>),
    Call2(MathFunc, Box<Ast>, Box<Ast>),
    Call3(MathFunc, Box<Ast>, Box<Ast>, Box<Ast>),
}

/// Compact stack-bytecode instruction emitted by [`CompiledFormula`].
#[derive(Copy, Clone, Debug)]
enum Instr {
    PushConst(f64),
    LoadChannel(usize),
    PushTimeSec,
    Unary(UnaryOp),
    Binary(BinaryOp),
    Call1(MathFunc),
    Call2(MathFunc),
    Call3(MathFunc),
    JumpIfFalse(usize),
    JumpIfTrue(usize),
    Jump(usize),
}

/// Compiled mathematical formula with resolved input channel slots and reusable stack execution.
#[derive(Clone, Debug)]
pub struct CompiledFormula {
    channels: Vec<String>,
    code: Vec<Instr>,
}

impl CompiledFormula {
    /// Parses, validates, constant-folds, and compiles an expression string into stack bytecode.
    pub fn compile(expression: &str) -> CoreResult<Self> {
        let trimmed = expression.trim();
        if trimmed.is_empty() {
            return Err(CoreError::InvalidConfig(
                "formula expression must not be empty".into(),
            ));
        }

        let tokens = tokenize(trimmed)?;
        let mut parser = Parser {
            tokens: &tokens,
            pos: 0,
            channels: Vec::new(),
        };
        let ast = parser.parse_expr()?;
        if parser.pos < parser.tokens.len() {
            return Err(CoreError::InvalidConfig(format!(
                "unexpected trailing token `{:?}` in formula `{trimmed}`",
                parser.tokens[parser.pos]
            )));
        }

        let folded = fold_ast(ast);
        let mut code = Vec::with_capacity(32);
        emit_bytecode(&folded, &mut code);

        Ok(Self {
            channels: parser.channels,
            code,
        })
    }

    /// Ordered unique list of referenced `<source_id>/<local_path>` channel paths.
    #[inline]
    pub fn input_channels(&self) -> &[String] {
        &self.channels
    }

    /// Evaluates the compiled bytecode for sample timestamp `ts_ns` and aligned `channel_values`.
    ///
    /// Non-finite (`NaN`/`Inf`) outcomes are coerced to `0.0`.
    #[inline]
    pub fn eval(
        &self,
        ts_ns: TimestampNs,
        origin_ns: TimestampNs,
        channel_values: &[f64],
        stack: &mut Vec<f64>,
    ) -> f64 {
        stack.clear();
        let mut pc = 0usize;
        let len = self.code.len();
        while pc < len {
            match self.code[pc] {
                Instr::PushConst(v) => stack.push(v),
                Instr::LoadChannel(idx) => {
                    let v = channel_values.get(idx).copied().unwrap_or(0.0);
                    stack.push(if v.is_finite() { v } else { 0.0 });
                }
                Instr::PushTimeSec => {
                    let sec = ts_ns.saturating_sub(origin_ns) as f64 * 1e-9;
                    stack.push(sec);
                }
                Instr::Unary(op) => {
                    let a = stack.pop().unwrap_or(0.0);
                    let res = match op {
                        UnaryOp::Neg => -a,
                        UnaryOp::Not => {
                            if a == 0.0 {
                                1.0
                            } else {
                                0.0
                            }
                        }
                    };
                    stack.push(sanitize_f64(res));
                }
                Instr::Binary(op) => {
                    let b = stack.pop().unwrap_or(0.0);
                    let a = stack.pop().unwrap_or(0.0);
                    let res = eval_binary(op, a, b);
                    stack.push(sanitize_f64(res));
                }
                Instr::Call1(func) => {
                    let a = stack.pop().unwrap_or(0.0);
                    stack.push(sanitize_f64(func.eval_1(a)));
                }
                Instr::Call2(func) => {
                    let b = stack.pop().unwrap_or(0.0);
                    let a = stack.pop().unwrap_or(0.0);
                    stack.push(sanitize_f64(func.eval_2(a, b)));
                }
                Instr::Call3(func) => {
                    let c = stack.pop().unwrap_or(0.0);
                    let b = stack.pop().unwrap_or(0.0);
                    let a = stack.pop().unwrap_or(0.0);
                    stack.push(sanitize_f64(func.eval_3(a, b, c)));
                }
                Instr::JumpIfFalse(target) => {
                    let cond = stack.pop().unwrap_or(0.0);
                    if cond == 0.0 {
                        pc = target;
                        continue;
                    }
                }
                Instr::JumpIfTrue(target) => {
                    let cond = stack.pop().unwrap_or(0.0);
                    if cond != 0.0 {
                        pc = target;
                        continue;
                    }
                }
                Instr::Jump(target) => {
                    pc = target;
                    continue;
                }
            }
            pc += 1;
        }
        sanitize_f64(stack.pop().unwrap_or(0.0))
    }
}

#[inline]
fn sanitize_f64(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

#[inline]
fn eval_binary(op: BinaryOp, a: f64, b: f64) -> f64 {
    match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => {
            if b == 0.0 {
                0.0
            } else {
                a / b
            }
        }
        BinaryOp::Rem => {
            if b == 0.0 {
                0.0
            } else {
                a % b
            }
        }
        BinaryOp::Pow => a.powf(b),
        BinaryOp::Lt => {
            if a < b {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::LtEq => {
            if a <= b {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::Gt => {
            if a > b {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::GtEq => {
            if a >= b {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::Eq => {
            if (a - b).abs() <= 1e-12 {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::NotEq => {
            if (a - b).abs() > 1e-12 {
                1.0
            } else {
                0.0
            }
        }
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_path_segment_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.'
}

fn tokenize(input: &str) -> CoreResult<Vec<Token>> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        if b == b'[' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b']' {
                end += 1;
            }
            if end >= bytes.len() {
                return Err(CoreError::InvalidConfig(
                    "unclosed '[' in formula channel reference".into(),
                ));
            }
            let inner = input[start..end].trim();
            if inner.is_empty() || !inner.contains('/') {
                return Err(CoreError::InvalidConfig(format!(
                    "bracketed channel reference `[{inner}]` must be `<source>/<path>`"
                )));
            }
            tokens.push(Token::ChannelRef(inner.to_owned()));
            i = end + 1;
            continue;
        }

        if b.is_ascii_digit()
            || (b == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit())
        {
            let start = i;
            if b == b'0' && i + 1 < bytes.len() && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X') {
                i += 2;
                let hex_start = i;
                while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                    i += 1;
                }
                if i == hex_start {
                    return Err(CoreError::InvalidConfig(
                        "invalid hex literal in formula".into(),
                    ));
                }
                let val = u64::from_str_radix(&input[hex_start..i], 16).map_err(|e| {
                    CoreError::InvalidConfig(format!("invalid hex literal in formula: {e}"))
                })?;
                tokens.push(Token::Number(val as f64));
                continue;
            }

            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                i += 1;
                if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                    i += 1;
                }
                let exp_start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == exp_start {
                    return Err(CoreError::InvalidConfig(format!(
                        "invalid scientific exponent in `{}`",
                        &input[start..i]
                    )));
                }
            }
            let raw = &input[start..i];
            let num: f64 = raw.parse().map_err(|e| {
                CoreError::InvalidConfig(format!("invalid numeric literal `{raw}`: {e}"))
            })?;
            tokens.push(Token::Number(num));
            continue;
        }

        if is_ident_start(b) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            let ident = &input[start..i];

            let mut look = i;
            while look < bytes.len() && bytes[look].is_ascii_whitespace() {
                look += 1;
            }
            let followed_by_paren = look < bytes.len() && bytes[look] == b'(';
            let is_reserved_var = matches!(ident, "t" | "pi" | "e" | "tau");

            if !is_reserved_var
                && !followed_by_paren
                && i + 1 < bytes.len()
                && bytes[i] == b'/'
                && is_ident_start(bytes[i + 1])
            {
                while i + 1 < bytes.len() && bytes[i] == b'/' && is_ident_start(bytes[i + 1]) {
                    i += 1; // skip '/'
                    i += 1; // first char of segment
                    while i < bytes.len() && is_path_segment_continue(bytes[i]) {
                        i += 1;
                    }
                }
                tokens.push(Token::ChannelRef(input[start..i].to_owned()));
                continue;
            }

            tokens.push(Token::Ident(ident.to_owned()));
            continue;
        }

        if i + 1 < bytes.len() {
            let two = &input[i..i + 2];
            let matched = match two {
                "==" => Some(Token::EqEq),
                "!=" => Some(Token::NotEq),
                "<=" => Some(Token::LtEq),
                ">=" => Some(Token::GtEq),
                "&&" => Some(Token::AndAnd),
                "||" => Some(Token::OrOr),
                _ => None,
            };
            if let Some(tok) = matched {
                tokens.push(tok);
                i += 2;
                continue;
            }
        }

        let single = match b {
            b'+' => Token::Plus,
            b'-' => Token::Minus,
            b'*' => Token::Star,
            b'/' => Token::Slash,
            b'%' => Token::Percent,
            b'^' => Token::Caret,
            b'!' => Token::Bang,
            b'<' => Token::Lt,
            b'>' => Token::Gt,
            b'?' => Token::Question,
            b':' => Token::Colon,
            b'(' => Token::LParen,
            b')' => Token::RParen,
            b',' => Token::Comma,
            _ => {
                return Err(CoreError::InvalidConfig(format!(
                    "unexpected character `{}` in formula at byte {i}",
                    input[i..].chars().next().unwrap_or('?')
                )));
            }
        };
        tokens.push(single);
        i += 1;
    }

    Ok(tokens)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    channels: Vec<String>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(tok)
    }

    fn intern_channel(&mut self, path: &str) -> usize {
        if let Some(idx) = self.channels.iter().position(|p| p == path) {
            idx
        } else {
            let idx = self.channels.len();
            self.channels.push(path.to_owned());
            idx
        }
    }

    fn expect(&mut self, expected: &Token, msg: &str) -> CoreResult<()> {
        if self.peek() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(CoreError::InvalidConfig(msg.into()))
        }
    }

    fn parse_expr(&mut self) -> CoreResult<Ast> {
        self.parse_ternary()
    }

    fn parse_ternary(&mut self) -> CoreResult<Ast> {
        let cond = self.parse_or()?;
        if self.peek() == Some(&Token::Question) {
            self.pos += 1;
            let then_branch = self.parse_expr()?;
            self.expect(&Token::Colon, "expected ':' in ternary expression")?;
            let else_branch = self.parse_ternary()?;
            return Ok(Ast::Ternary(
                Box::new(cond),
                Box::new(then_branch),
                Box::new(else_branch),
            ));
        }
        Ok(cond)
    }

    fn parse_or(&mut self) -> CoreResult<Ast> {
        let mut lhs = self.parse_and()?;
        while self.peek() == Some(&Token::OrOr) {
            self.pos += 1;
            let rhs = self.parse_and()?;
            lhs = Ast::LogicalOr(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> CoreResult<Ast> {
        let mut lhs = self.parse_equality()?;
        while self.peek() == Some(&Token::AndAnd) {
            self.pos += 1;
            let rhs = self.parse_equality()?;
            lhs = Ast::LogicalAnd(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> CoreResult<Ast> {
        let mut lhs = self.parse_comparison()?;
        loop {
            let op = match self.peek() {
                Some(Token::EqEq) => BinaryOp::Eq,
                Some(Token::NotEq) => BinaryOp::NotEq,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_comparison()?;
            lhs = Ast::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_comparison(&mut self) -> CoreResult<Ast> {
        let mut lhs = self.parse_additive()?;
        loop {
            let op = match self.peek() {
                Some(Token::Lt) => BinaryOp::Lt,
                Some(Token::LtEq) => BinaryOp::LtEq,
                Some(Token::Gt) => BinaryOp::Gt,
                Some(Token::GtEq) => BinaryOp::GtEq,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_additive()?;
            lhs = Ast::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> CoreResult<Ast> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => BinaryOp::Add,
                Some(Token::Minus) => BinaryOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_multiplicative()?;
            lhs = Ast::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> CoreResult<Ast> {
        let mut lhs = self.parse_power()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => BinaryOp::Mul,
                Some(Token::Slash) => BinaryOp::Div,
                Some(Token::Percent) => BinaryOp::Rem,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_power()?;
            lhs = Ast::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_power(&mut self) -> CoreResult<Ast> {
        let lhs = self.parse_unary()?;
        if self.peek() == Some(&Token::Caret) {
            self.pos += 1;
            let rhs = self.parse_power()?;
            return Ok(Ast::Binary(BinaryOp::Pow, Box::new(lhs), Box::new(rhs)));
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> CoreResult<Ast> {
        match self.peek() {
            Some(Token::Plus) => {
                self.pos += 1;
                self.parse_unary()
            }
            Some(Token::Minus) => {
                self.pos += 1;
                let inner = self.parse_unary()?;
                Ok(Ast::Unary(UnaryOp::Neg, Box::new(inner)))
            }
            Some(Token::Bang) => {
                self.pos += 1;
                let inner = self.parse_unary()?;
                Ok(Ast::Unary(UnaryOp::Not, Box::new(inner)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> CoreResult<Ast> {
        let tok = self
            .next()
            .cloned()
            .ok_or_else(|| CoreError::InvalidConfig("unexpected end of formula".into()))?;
        match tok {
            Token::Number(n) => Ok(Ast::Const(n)),
            Token::ChannelRef(path) => {
                let idx = self.intern_channel(&path);
                Ok(Ast::Channel(idx))
            }
            Token::Ident(name) => {
                if self.peek() == Some(&Token::LParen) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if self.peek() != Some(&Token::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if self.peek() == Some(&Token::Comma) {
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(
                        &Token::RParen,
                        &format!("expected ')' after arguments of `{name}(...)`"),
                    )?;

                    let Some((func, arity)) = MathFunc::from_name(&name) else {
                        return Err(CoreError::InvalidConfig(format!(
                            "unknown formula function `{name}`"
                        )));
                    };
                    if args.len() != arity {
                        return Err(CoreError::InvalidConfig(format!(
                            "function `{name}` expects {arity} argument(s), got {}",
                            args.len()
                        )));
                    }
                    let mut it = args.into_iter();
                    return Ok(match arity {
                        1 => Ast::Call1(func, Box::new(it.next().unwrap())),
                        2 => Ast::Call2(
                            func,
                            Box::new(it.next().unwrap()),
                            Box::new(it.next().unwrap()),
                        ),
                        3 => Ast::Call3(
                            func,
                            Box::new(it.next().unwrap()),
                            Box::new(it.next().unwrap()),
                            Box::new(it.next().unwrap()),
                        ),
                        _ => unreachable!(),
                    });
                }

                match name.as_str() {
                    "t" => Ok(Ast::TimeSec),
                    "pi" => Ok(Ast::Const(std::f64::consts::PI)),
                    "tau" => Ok(Ast::Const(std::f64::consts::TAU)),
                    "e" => Ok(Ast::Const(std::f64::consts::E)),
                    _ => Err(CoreError::InvalidConfig(format!(
                        "unknown identifier `{name}` in formula (use `<source>/<channel>` for channels)"
                    ))),
                }
            }
            Token::LParen => {
                let expr = self.parse_expr()?;
                self.expect(&Token::RParen, "expected matching ')' in formula")?;
                Ok(expr)
            }
            other => Err(CoreError::InvalidConfig(format!(
                "unexpected token `{other:?}` in formula"
            ))),
        }
    }
}

fn fold_ast(ast: Ast) -> Ast {
    match ast {
        Ast::Const(_) | Ast::Channel(_) | Ast::TimeSec => ast,
        Ast::Unary(op, inner) => {
            let inner = fold_ast(*inner);
            if let Ast::Const(a) = inner {
                let res = match op {
                    UnaryOp::Neg => -a,
                    UnaryOp::Not => {
                        if a == 0.0 {
                            1.0
                        } else {
                            0.0
                        }
                    }
                };
                Ast::Const(sanitize_f64(res))
            } else {
                Ast::Unary(op, Box::new(inner))
            }
        }
        Ast::Binary(op, lhs, rhs) => {
            let lhs = fold_ast(*lhs);
            let rhs = fold_ast(*rhs);
            if let (Ast::Const(a), Ast::Const(b)) = (&lhs, &rhs) {
                Ast::Const(sanitize_f64(eval_binary(op, *a, *b)))
            } else {
                Ast::Binary(op, Box::new(lhs), Box::new(rhs))
            }
        }
        Ast::LogicalAnd(lhs, rhs) => {
            let lhs = fold_ast(*lhs);
            let rhs = fold_ast(*rhs);
            match lhs {
                Ast::Const(a) if a == 0.0 => Ast::Const(0.0),
                Ast::Const(_) => match rhs {
                    Ast::Const(b) => Ast::Const(if b != 0.0 { 1.0 } else { 0.0 }),
                    other => Ast::LogicalAnd(Box::new(Ast::Const(1.0)), Box::new(other)),
                },
                other => Ast::LogicalAnd(Box::new(other), Box::new(rhs)),
            }
        }
        Ast::LogicalOr(lhs, rhs) => {
            let lhs = fold_ast(*lhs);
            let rhs = fold_ast(*rhs);
            match lhs {
                Ast::Const(a) if a != 0.0 => Ast::Const(1.0),
                Ast::Const(_) => match rhs {
                    Ast::Const(b) => Ast::Const(if b != 0.0 { 1.0 } else { 0.0 }),
                    other => Ast::LogicalOr(Box::new(Ast::Const(0.0)), Box::new(other)),
                },
                other => Ast::LogicalOr(Box::new(other), Box::new(rhs)),
            }
        }
        Ast::Ternary(cond, then_b, else_b) => {
            let cond = fold_ast(*cond);
            let then_b = fold_ast(*then_b);
            let else_b = fold_ast(*else_b);
            match cond {
                Ast::Const(c) => {
                    if c != 0.0 {
                        then_b
                    } else {
                        else_b
                    }
                }
                other => Ast::Ternary(Box::new(other), Box::new(then_b), Box::new(else_b)),
            }
        }
        Ast::Call1(func, a) => {
            let a = fold_ast(*a);
            if let Ast::Const(va) = a {
                Ast::Const(sanitize_f64(func.eval_1(va)))
            } else {
                Ast::Call1(func, Box::new(a))
            }
        }
        Ast::Call2(func, a, b) => {
            let a = fold_ast(*a);
            let b = fold_ast(*b);
            if let (Ast::Const(va), Ast::Const(vb)) = (&a, &b) {
                Ast::Const(sanitize_f64(func.eval_2(*va, *vb)))
            } else {
                Ast::Call2(func, Box::new(a), Box::new(b))
            }
        }
        Ast::Call3(func, a, b, c) => {
            let a = fold_ast(*a);
            let b = fold_ast(*b);
            let c = fold_ast(*c);
            if let (Ast::Const(va), Ast::Const(vb), Ast::Const(vc)) = (&a, &b, &c) {
                Ast::Const(sanitize_f64(func.eval_3(*va, *vb, *vc)))
            } else {
                Ast::Call3(func, Box::new(a), Box::new(b), Box::new(c))
            }
        }
    }
}

fn emit_bytecode(ast: &Ast, code: &mut Vec<Instr>) {
    match ast {
        Ast::Const(v) => code.push(Instr::PushConst(*v)),
        Ast::Channel(idx) => code.push(Instr::LoadChannel(*idx)),
        Ast::TimeSec => code.push(Instr::PushTimeSec),
        Ast::Unary(op, inner) => {
            emit_bytecode(inner, code);
            code.push(Instr::Unary(*op));
        }
        Ast::Binary(op, lhs, rhs) => {
            emit_bytecode(lhs, code);
            emit_bytecode(rhs, code);
            code.push(Instr::Binary(*op));
        }
        Ast::LogicalAnd(lhs, rhs) => {
            emit_bytecode(lhs, code);
            let j_false_idx = code.len();
            code.push(Instr::JumpIfFalse(0));
            emit_bytecode(rhs, code);
            code.push(Instr::PushConst(0.0));
            code.push(Instr::Binary(BinaryOp::NotEq));
            let j_end_idx = code.len();
            code.push(Instr::Jump(0));
            let false_target = code.len();
            code[j_false_idx] = Instr::JumpIfFalse(false_target);
            code.push(Instr::PushConst(0.0));
            let end_target = code.len();
            code[j_end_idx] = Instr::Jump(end_target);
        }
        Ast::LogicalOr(lhs, rhs) => {
            emit_bytecode(lhs, code);
            let j_true_idx = code.len();
            code.push(Instr::JumpIfTrue(0));
            emit_bytecode(rhs, code);
            code.push(Instr::PushConst(0.0));
            code.push(Instr::Binary(BinaryOp::NotEq));
            let j_end_idx = code.len();
            code.push(Instr::Jump(0));
            let true_target = code.len();
            code[j_true_idx] = Instr::JumpIfTrue(true_target);
            code.push(Instr::PushConst(1.0));
            let end_target = code.len();
            code[j_end_idx] = Instr::Jump(end_target);
        }
        Ast::Ternary(cond, then_b, else_b) => {
            emit_bytecode(cond, code);
            let j_false_idx = code.len();
            code.push(Instr::JumpIfFalse(0));
            emit_bytecode(then_b, code);
            let j_end_idx = code.len();
            code.push(Instr::Jump(0));
            let else_target = code.len();
            code[j_false_idx] = Instr::JumpIfFalse(else_target);
            emit_bytecode(else_b, code);
            let end_target = code.len();
            code[j_end_idx] = Instr::Jump(end_target);
        }
        Ast::Call1(func, a) => {
            emit_bytecode(a, code);
            code.push(Instr::Call1(*func));
        }
        Ast::Call2(func, a, b) => {
            emit_bytecode(a, code);
            emit_bytecode(b, code);
            code.push(Instr::Call2(*func));
        }
        Ast::Call3(func, a, b, c) => {
            emit_bytecode(a, code);
            emit_bytecode(b, code);
            emit_bytecode(c, code);
            code.push(Instr::Call3(*func));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_compiles_channel_refs_math_and_ternary() {
        let f = CompiledFormula::compile(
            "sim0/v_bus * [sim0/ia] + hypot(sim0/ia, sim0/ib) - (sim0/v_bus/2) + (t > 0.5 ? 10 : -5)",
        )
        .unwrap();
        assert_eq!(f.input_channels(), &["sim0/v_bus", "sim0/ia", "sim0/ib"]);

        let mut stack = Vec::new();
        // v_bus = 48, ia = 3, ib = 4 => 48*3 + 5 - 24 + (-5) = 144 + 5 - 24 - 5 = 120
        let v0 = f.eval(200_000_000, 0, &[48.0, 3.0, 4.0], &mut stack);
        assert!((v0 - 120.0).abs() < 1e-9);

        // t = 1.0s > 0.5 => +10 instead of -5 => 135
        let v1 = f.eval(1_000_000_000, 0, &[48.0, 3.0, 4.0], &mut stack);
        assert!((v1 - 135.0).abs() < 1e-9);
    }

    #[test]
    fn formula_division_by_zero_and_nan_coercion_is_safe() {
        let f = CompiledFormula::compile("sim0/v_bus / 0 + sqrt(-4)").unwrap();
        let mut stack = Vec::new();
        assert_eq!(f.eval(0, 0, &[10.0], &mut stack), 0.0);
    }
}
