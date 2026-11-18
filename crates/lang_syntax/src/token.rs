use lang_diag::Span;
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// One piece of an f-string.
#[derive(Clone, Debug, PartialEq)]
pub enum FPiece {
    Literal(String),
    /// The source text of `{expression}` and the byte offset in the file where it starts, so
    /// the parser can lex and parse it with correct positions. `spec` is the text after `:`, with
    /// its offset.
    Code {
        text: String,
        offset: usize,
        spec: Option<(String, usize)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Name(String),
    Int(u64),
    Float(f64),
    Str(String),
    /// An f-string: literal text and `{expression:spec}` pieces.
    FString(Vec<FPiece>),

    // Layout, produced from line breaks and indentation.
    Newline,
    Indent,
    Dedent,
    Eof,

    // Keywords.
    And,
    As,
    Break,
    Case,
    Const,
    Continue,
    Def,
    Elif,
    Else,
    Enum,
    False,
    For,
    From,
    If,
    Impl,
    Import,
    In,
    Is,
    Lambda,
    Match,
    NoneKw,
    Not,
    Or,
    Pass,
    Priv,
    Pub,
    Return,
    SelfKw,
    Spawn,
    Struct,
    Trait,
    True,
    While,
    With,

    // Punctuation and operators.
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Dot,
    Arrow,
    At,
    Question,
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    SlashSlashEq,
    PercentEq,
    Plus,
    Minus,
    Star,
    StarStar,
    Slash,
    SlashSlash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
}

/// The keyword spelled `word`, if it is one.
pub fn keyword(word: &str) -> Option<TokenKind> {
    use TokenKind::*;
    Some(match word {
        "and" => And,
        "as" => As,
        "break" => Break,
        "case" => Case,
        "const" => Const,
        "continue" => Continue,
        "def" => Def,
        "elif" => Elif,
        "else" => Else,
        "enum" => Enum,
        "False" => False,
        "for" => For,
        "from" => From,
        "if" => If,
        "impl" => Impl,
        "import" => Import,
        "in" => In,
        "is" => Is,
        "lambda" => Lambda,
        "match" => Match,
        "None" => NoneKw,
        "not" => Not,
        "or" => Or,
        "pass" => Pass,
        "priv" => Priv,
        "pub" => Pub,
        "return" => Return,
        "self" => SelfKw,
        "spawn" => Spawn,
        "struct" => Struct,
        "trait" => Trait,
        "True" => True,
        "while" => While,
        "with" => With,
        _ => return None,
    })
}

impl TokenKind {
    /// Source spelling of keywords and punctuation; `None` for tokens that carry a value or
    /// are synthesised from layout.
    pub fn text(&self) -> Option<&'static str> {
        use TokenKind::*;
        Some(match self {
            Name(_) | Int(_) | Float(_) | Str(_) | FString(_) | Newline | Indent | Dedent | Eof => return None,
            And => "and",
            As => "as",
            Break => "break",
            Case => "case",
            Const => "const",
            Continue => "continue",
            Def => "def",
            Elif => "elif",
            Else => "else",
            Enum => "enum",
            False => "False",
            For => "for",
            From => "from",
            If => "if",
            Impl => "impl",
            Import => "import",
            In => "in",
            Is => "is",
            Lambda => "lambda",
            Match => "match",
            NoneKw => "None",
            Not => "not",
            Or => "or",
            Pass => "pass",
            Priv => "priv",
            Pub => "pub",
            Return => "return",
            SelfKw => "self",
            Spawn => "spawn",
            Struct => "struct",
            Trait => "trait",
            True => "True",
            While => "while",
            With => "with",
            LParen => "(",
            RParen => ")",
            LBracket => "[",
            RBracket => "]",
            LBrace => "{",
            RBrace => "}",
            Colon => ":",
            Comma => ",",
            Dot => ".",
            Arrow => "->",
            At => "@",
            Question => "?",
            Assign => "=",
            PlusEq => "+=",
            MinusEq => "-=",
            StarEq => "*=",
            SlashEq => "/=",
            SlashSlashEq => "//=",
            PercentEq => "%=",
            Plus => "+",
            Minus => "-",
            Star => "*",
            StarStar => "**",
            Slash => "/",
            SlashSlash => "//",
            Percent => "%",
            Amp => "&",
            Pipe => "|",
            Caret => "^",
            Tilde => "~",
            Shl => "<<",
            Shr => ">>",
            EqEq => "==",
            NotEq => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
        })
    }

    /// How the token is named in error messages: "`def`", "name `x`", "end of line".
    pub fn describe(&self) -> String {
        match self {
            TokenKind::Name(name) => format!("name `{name}`"),
            TokenKind::Int(value) => format!("number `{value}`"),
            TokenKind::Float(value) => format!("number `{value}`"),
            TokenKind::Str(_) => "string".to_string(),
            TokenKind::FString(_) => "f-string".to_string(),
            TokenKind::Newline => "end of line".to_string(),
            TokenKind::Indent => "indented block".to_string(),
            TokenKind::Dedent => "end of block".to_string(),
            TokenKind::Eof => "end of file".to_string(),
            other => format!("`{}`", other.text().unwrap_or_default()),
        }
    }
}

/// The form used by `--emit=tokens`.
impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Name(name) => write!(f, "Name({name})"),
            TokenKind::Int(value) => write!(f, "Int({value})"),
            TokenKind::Float(value) => write!(f, "Float({value:?})"),
            TokenKind::Str(value) => write!(f, "Str({value:?})"),
            TokenKind::FString(pieces) => write!(f, "FString({} pieces)", pieces.len()),
            TokenKind::Newline => f.write_str("NEWLINE"),
            TokenKind::Indent => f.write_str("INDENT"),
            TokenKind::Dedent => f.write_str("DEDENT"),
            TokenKind::Eof => f.write_str("EOF"),
            other => f.write_str(other.text().unwrap_or_default()),
        }
    }
}
