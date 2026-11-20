" Vim syntax file for Weby (.wb). Install by copying `ftdetect/` and `syntax/` into ~/.vim
" (or ~/.config/nvim).
if exists("b:current_syntax")
  finish
endif

syn keyword webyStatement  return pass break continue assert spawn lambda
syn keyword webyConditional if elif else match case
syn keyword webyRepeat     for while
syn keyword webyOperator   and or not in is
syn keyword webyInclude    import from as
syn keyword webyStorage    def struct enum trait impl type const pub priv with
syn keyword webyConstant   True False None
syn keyword webySelf       self
syn keyword webyType       int float bool str bytes list dict tuple frozendict Result Task Channel Mutex Atomic
syn keyword webyType       i8 i16 i32 i64 u8 u16 u32 u64 f32 f64
syn keyword webyBuiltin    Some Ok Err print len range min max sum sorted enumerate zip any all panic

syn match   webyAnnotation "^\s*@[A-Za-z_][A-Za-z0-9_.]*"
syn match   webyNumber     "\<\d[0-9_]*\(\.[0-9_]\+\)\=\([eE][+-]\=[0-9_]\+\)\=\>"
syn match   webyNumber     "\<0[xX][0-9A-Fa-f_]\+\>"
syn match   webyFunction   "\(\<def\s\+\)\@<=[A-Za-z_][A-Za-z0-9_]*"
syn match   webyTypeName   "\<[A-Z][A-Za-z0-9_]*\>"
syn match   webyComment    "#.*$" contains=@Spell

syn region  webyDocString  start=+"""+ end=+"""+ contains=@Spell
syn region  webyString     start=+[fFrR]\="+ skip=+\\\\\|\\"+ end=+"+ oneline
syn region  webyString     start=+[fFrR]\='+ skip=+\\\\\|\\'+ end=+'+ oneline

hi def link webyStatement   Statement
hi def link webyConditional Conditional
hi def link webyRepeat      Repeat
hi def link webyOperator    Operator
hi def link webyInclude     Include
hi def link webyStorage     StorageClass
hi def link webyConstant    Constant
hi def link webySelf        Identifier
hi def link webyType        Type
hi def link webyTypeName    Type
hi def link webyBuiltin     Function
hi def link webyAnnotation  PreProc
hi def link webyNumber      Number
hi def link webyFunction    Function
hi def link webyComment     Comment
hi def link webyDocString   String
hi def link webyString      String

let b:current_syntax = "weby"
