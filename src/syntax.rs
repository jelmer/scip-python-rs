use ruff_python_ast::token::{Token, TokenKind};
use ruff_text_size::Ranged;
use scip::types::SyntaxKind;

/// Python names that are always available without being bound anywhere, and
/// so never resolve to a symbol. Highlighted as builtins rather than plain
/// identifiers. Types are separated out so they can be given the more
/// specific `IdentifierBuiltinType`.
const BUILTIN_TYPES: &[&str] = &[
    "bool",
    "bytearray",
    "bytes",
    "complex",
    "dict",
    "float",
    "frozenset",
    "int",
    "list",
    "memoryview",
    "object",
    "set",
    "str",
    "tuple",
    "type",
    "ArithmeticError",
    "AssertionError",
    "AttributeError",
    "BaseException",
    "BaseExceptionGroup",
    "BlockingIOError",
    "BrokenPipeError",
    "BufferError",
    "BytesWarning",
    "ChildProcessError",
    "ConnectionAbortedError",
    "ConnectionError",
    "ConnectionRefusedError",
    "ConnectionResetError",
    "DeprecationWarning",
    "EOFError",
    "EncodingWarning",
    "EnvironmentError",
    "Exception",
    "ExceptionGroup",
    "FileExistsError",
    "FileNotFoundError",
    "FloatingPointError",
    "FutureWarning",
    "GeneratorExit",
    "IOError",
    "ImportError",
    "ImportWarning",
    "IndentationError",
    "IndexError",
    "InterruptedError",
    "IsADirectoryError",
    "KeyError",
    "KeyboardInterrupt",
    "LookupError",
    "MemoryError",
    "ModuleNotFoundError",
    "NameError",
    "NotADirectoryError",
    "NotImplementedError",
    "OSError",
    "OverflowError",
    "PendingDeprecationWarning",
    "PermissionError",
    "ProcessLookupError",
    "RecursionError",
    "ReferenceError",
    "ResourceWarning",
    "RuntimeError",
    "RuntimeWarning",
    "StopAsyncIteration",
    "StopIteration",
    "SyntaxError",
    "SyntaxWarning",
    "SystemError",
    "SystemExit",
    "TabError",
    "TimeoutError",
    "TypeError",
    "UnboundLocalError",
    "UnicodeDecodeError",
    "UnicodeEncodeError",
    "UnicodeError",
    "UnicodeTranslateError",
    "UnicodeWarning",
    "UserWarning",
    "ValueError",
    "Warning",
    "ZeroDivisionError",
];

const BUILTIN_FUNCTIONS: &[&str] = &[
    "abs",
    "aiter",
    "anext",
    "all",
    "any",
    "ascii",
    "bin",
    "breakpoint",
    "callable",
    "chr",
    "classmethod",
    "compile",
    "delattr",
    "dir",
    "divmod",
    "enumerate",
    "eval",
    "exec",
    "filter",
    "format",
    "getattr",
    "globals",
    "hasattr",
    "hash",
    "help",
    "hex",
    "id",
    "input",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "locals",
    "map",
    "max",
    "min",
    "next",
    "oct",
    "open",
    "ord",
    "pow",
    "print",
    "property",
    "range",
    "repr",
    "reversed",
    "round",
    "setattr",
    "slice",
    "sorted",
    "staticmethod",
    "sum",
    "super",
    "vars",
    "zip",
    "__import__",
];

/// Names that are keyword-like but lex as plain identifiers.
const BUILTIN_CONSTANTS: &[&str] = &["NotImplemented", "Ellipsis", "__debug__"];

/// The syntax kind for a `Name` token that the indexer could not resolve to
/// a symbol. Only builtins are worth distinguishing; anything else is just
/// an identifier. `self` and `cls` do not need a case here: in a method they
/// resolve to the parameter they are bound to.
pub fn unresolved_name_kind(name: &str) -> SyntaxKind {
    if BUILTIN_TYPES.contains(&name) {
        SyntaxKind::IdentifierBuiltinType
    } else if BUILTIN_FUNCTIONS.contains(&name) {
        SyntaxKind::IdentifierBuiltin
    } else if BUILTIN_CONSTANTS.contains(&name) {
        SyntaxKind::IdentifierConstant
    } else {
        SyntaxKind::Identifier
    }
}

/// The syntax kind for a token that carries no symbol, i.e. everything but
/// identifiers. `None` for tokens with no visible extent (indentation,
/// newlines, end of file), which must not produce an occurrence.
///
/// `Name` is deliberately absent: the caller decides between the resolved
/// and unresolved identifier kinds.
pub fn token_kind(kind: TokenKind) -> Option<SyntaxKind> {
    let syntax = match kind {
        TokenKind::Comment => SyntaxKind::Comment,

        TokenKind::Int | TokenKind::Float | TokenKind::Complex => SyntaxKind::NumericLiteral,
        TokenKind::True | TokenKind::False => SyntaxKind::BooleanLiteral,
        TokenKind::None => SyntaxKind::IdentifierNull,

        // The start and end tokens carry the prefix and quotes; the middle
        // tokens are the literal text between interpolations. The
        // interpolated expressions lex as ordinary tokens in between.
        TokenKind::String
        | TokenKind::FStringStart
        | TokenKind::FStringMiddle
        | TokenKind::FStringEnd
        | TokenKind::TStringStart
        | TokenKind::TStringMiddle
        | TokenKind::TStringEnd => SyntaxKind::StringLiteral,

        TokenKind::Lpar
        | TokenKind::Rpar
        | TokenKind::Lsqb
        | TokenKind::Rsqb
        | TokenKind::Lbrace
        | TokenKind::Rbrace => SyntaxKind::PunctuationBracket,

        TokenKind::Colon | TokenKind::Comma | TokenKind::Semi | TokenKind::Dot => {
            SyntaxKind::PunctuationDelimiter
        }

        // `and`, `or`, `not`, `in` and `is` are operators spelled as words;
        // SCIP has no separate kind for them, so they stay keywords. The
        // soft keywords (`case`, `match`, `type`, `lazy`) only reach here
        // where the grammar treats them as keywords; elsewhere they lex as
        // `Name`. Listed out rather than using `is_keyword()` so that a new
        // keyword upstream is a compile error instead of a silent fallthrough.
        TokenKind::And
        | TokenKind::As
        | TokenKind::Assert
        | TokenKind::Async
        | TokenKind::Await
        | TokenKind::Break
        | TokenKind::Case
        | TokenKind::Class
        | TokenKind::Continue
        | TokenKind::Def
        | TokenKind::Del
        | TokenKind::Elif
        | TokenKind::Else
        | TokenKind::Except
        | TokenKind::Finally
        | TokenKind::For
        | TokenKind::From
        | TokenKind::Global
        | TokenKind::If
        | TokenKind::Import
        | TokenKind::In
        | TokenKind::Is
        | TokenKind::Lambda
        | TokenKind::Lazy
        | TokenKind::Match
        | TokenKind::Nonlocal
        | TokenKind::Not
        | TokenKind::Or
        | TokenKind::Pass
        | TokenKind::Raise
        | TokenKind::Return
        | TokenKind::Try
        | TokenKind::Type
        | TokenKind::While
        | TokenKind::With
        | TokenKind::Yield => SyntaxKind::IdentifierKeyword,

        TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::DoubleStar
        | TokenKind::Slash
        | TokenKind::DoubleSlash
        | TokenKind::Percent
        | TokenKind::At
        | TokenKind::Vbar
        | TokenKind::Amper
        | TokenKind::CircumFlex
        | TokenKind::Tilde
        | TokenKind::LeftShift
        | TokenKind::RightShift
        | TokenKind::Less
        | TokenKind::Greater
        | TokenKind::LessEqual
        | TokenKind::GreaterEqual
        | TokenKind::EqEqual
        | TokenKind::NotEqual
        | TokenKind::Equal
        | TokenKind::ColonEqual
        | TokenKind::PlusEqual
        | TokenKind::MinusEqual
        | TokenKind::StarEqual
        | TokenKind::DoubleStarEqual
        | TokenKind::SlashEqual
        | TokenKind::DoubleSlashEqual
        | TokenKind::PercentEqual
        | TokenKind::AtEqual
        | TokenKind::VbarEqual
        | TokenKind::AmperEqual
        | TokenKind::CircumflexEqual
        | TokenKind::LeftShiftEqual
        | TokenKind::RightShiftEqual
        | TokenKind::Rarrow
        | TokenKind::Exclamation
        | TokenKind::Question => SyntaxKind::IdentifierOperator,

        TokenKind::Ellipsis => SyntaxKind::IdentifierConstant,

        TokenKind::IpyEscapeCommand => SyntaxKind::IdentifierMacro,

        // Structural tokens with no visible extent, and names, which the
        // caller handles.
        TokenKind::Name
        | TokenKind::Newline
        | TokenKind::NonLogicalNewline
        | TokenKind::Indent
        | TokenKind::Dedent
        | TokenKind::EndOfFile
        | TokenKind::Unknown => return None,
    };
    Some(syntax)
}

/// Whether a token should be considered for an occurrence at all. Tokens
/// with an empty range (`Dedent`, and `Newline` at end of file) would emit a
/// zero-width occurrence, which no highlighter can use.
pub fn is_emittable(token: &Token) -> bool {
    !token.range().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_and_soft_keywords() {
        assert_eq!(
            token_kind(TokenKind::Def),
            Some(SyntaxKind::IdentifierKeyword)
        );
        assert_eq!(
            token_kind(TokenKind::And),
            Some(SyntaxKind::IdentifierKeyword)
        );
        // Soft keywords: `match`/`case`/`type` lex as keywords only where
        // the grammar allows them, and as `Name` elsewhere.
        assert_eq!(
            token_kind(TokenKind::Match),
            Some(SyntaxKind::IdentifierKeyword)
        );
    }

    #[test]
    fn literals() {
        assert_eq!(token_kind(TokenKind::Int), Some(SyntaxKind::NumericLiteral));
        assert_eq!(
            token_kind(TokenKind::True),
            Some(SyntaxKind::BooleanLiteral)
        );
        assert_eq!(
            token_kind(TokenKind::None),
            Some(SyntaxKind::IdentifierNull)
        );
        assert_eq!(
            token_kind(TokenKind::FStringMiddle),
            Some(SyntaxKind::StringLiteral)
        );
    }

    #[test]
    fn structural_tokens_have_no_kind() {
        assert_eq!(token_kind(TokenKind::Indent), None);
        assert_eq!(token_kind(TokenKind::Newline), None);
        assert_eq!(token_kind(TokenKind::EndOfFile), None);
        assert_eq!(token_kind(TokenKind::Name), None);
    }

    #[test]
    fn builtin_names() {
        assert_eq!(unresolved_name_kind("print"), SyntaxKind::IdentifierBuiltin);
        assert_eq!(
            unresolved_name_kind("int"),
            SyntaxKind::IdentifierBuiltinType
        );
        assert_eq!(
            unresolved_name_kind("NotImplemented"),
            SyntaxKind::IdentifierConstant
        );
        assert_eq!(unresolved_name_kind("whatever"), SyntaxKind::Identifier);
    }
}
