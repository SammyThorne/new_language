//! Stable diagnostic codes. A code is never reused for a different problem once released.
//!
//! E00xx lexical, E01xx syntax, E02xx names, E03xx types, W0xxx warnings.

/// A tab character in the indentation at the start of a line.
pub const TAB_IN_INDENT: &str = "E0001";
/// A line is less indented than its block but matches no enclosing block.
pub const BAD_DEDENT: &str = "E0002";
/// A string literal without its closing quote.
pub const UNTERMINATED_STRING: &str = "E0003";
/// An unknown backslash escape in a string literal.
pub const BAD_ESCAPE: &str = "E0004";
/// A character that cannot start any token.
pub const UNEXPECTED_CHAR: &str = "E0005";
/// An integer literal that does not fit in 64 bits.
pub const INT_TOO_LARGE: &str = "E0006";
/// A malformed number literal.
pub const BAD_NUMBER: &str = "E0007";

/// The parser found a token it did not expect.
pub const UNEXPECTED_TOKEN: &str = "E0100";
/// Valid syntax for a feature this compiler does not implement yet.
pub const NOT_SUPPORTED_SYNTAX: &str = "E0101";
/// A recognised construct that the type checker does not implement yet.
pub const NOT_SUPPORTED: &str = "E0102";
/// `a < b < c`.
pub const COMPARISON_CHAIN: &str = "E0103";
/// `break` or `continue` outside a loop.
pub const OUTSIDE_LOOP: &str = "E0104";

/// A name that refers to nothing.
pub const UNKNOWN_NAME: &str = "E0200";
/// Two definitions of the same name.
pub const DUPLICATE_DEFINITION: &str = "E0201";
/// The program has no `main` function.
pub const NO_MAIN: &str = "E0202";
/// Calling something that is not a function, or assigning to a function.
pub const NOT_A_FUNCTION: &str = "E0203";
/// A variable read before its first assignment.
pub const USED_BEFORE_ASSIGNED: &str = "E0204";

/// A value of one type where another was expected.
pub const TYPE_MISMATCH: &str = "E0300";
/// A call with the wrong number of arguments.
pub const WRONG_ARG_COUNT: &str = "E0301";
/// A function without `->` returns a value.
pub const RETURN_VALUE_FROM_NONE: &str = "E0302";
/// A function with a return type can reach its end without returning.
pub const MISSING_RETURN: &str = "E0303";
/// A type hint names a type that does not exist.
pub const UNKNOWN_TYPE: &str = "E0304";
/// A parameter without a type hint.
pub const MISSING_TYPE_HINT: &str = "E0305";
/// `main` has parameters or a return type.
pub const BAD_MAIN_SIGNATURE: &str = "E0306";
/// A type hint on an assignment to a variable that already exists.
pub const REDECLARED_VARIABLE: &str = "E0307";
/// A variable whose value has type `None`, which carries no information.
pub const NONE_VARIABLE: &str = "E0308";
/// Type arguments on a type that takes none.
pub const UNEXPECTED_TYPE_ARGS: &str = "E0309";
/// A parameter whose type is `None`.
pub const NONE_PARAMETER: &str = "E0310";
/// A struct construction that leaves out a field without a default.
pub const MISSING_FIELD: &str = "E0311";
/// A constant whose value is not a literal.
pub const CONST_NOT_LITERAL: &str = "E0312";
/// A dict key type that cannot be hashed.
pub const UNHASHABLE_KEY: &str = "E0313";
/// An f-string format spec that is malformed or does not suit the value.
pub const BAD_FORMAT_SPEC: &str = "E0314";
/// A type that cannot be inferred, such as that of `[]` with no type hint.
pub const CANNOT_INFER: &str = "E0315";
/// A field or method that does not exist.
pub const NO_SUCH_MEMBER: &str = "E0316";
/// A condition that is not a `bool`.
pub const NOT_BOOL: &str = "E0317";
/// A `for` loop over something that cannot be iterated.
pub const NOT_ITERABLE: &str = "E0318";
/// Unpacking a value into the wrong number of names.
pub const UNPACK_MISMATCH: &str = "E0319";
/// Indexing something that cannot be indexed that way.
pub const BAD_INDEX: &str = "E0320";
/// An operator applied to types it does not support.
pub const BAD_OPERANDS: &str = "E0321";

/// A variable that is assigned but never read.
pub const UNUSED_VARIABLE: &str = "W0001";
/// Statements after a `return`.
pub const UNREACHABLE_CODE: &str = "W0002";
