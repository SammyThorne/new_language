" Vim syntax file for Viper (.vi). Install by copying `ftdetect/` and `syntax/` into ~/.vim
" (or ~/.config/nvim).
if exists("b:current_syntax")
  finish
endif

syn keyword viperStatement  return pass break continue assert spawn lambda
syn keyword viperConditional if elif else match case
syn keyword viperRepeat     for while
syn keyword viperOperator   and or not in is
syn keyword viperInclude    import from as
syn keyword viperStorage    def struct enum trait impl type const pub priv with
syn keyword viperConstant   True False None
syn keyword viperSelf       self
syn keyword viperType       int float bool str bytes list dict tuple frozendict Result Task Channel Mutex Atomic
syn keyword viperType       i8 i16 i32 i64 u8 u16 u32 u64 f32 f64
syn keyword viperBuiltin    Some Ok Err print len range min max sum sorted enumerate zip any all panic

syn match   viperAnnotation "^\s*@[A-Za-z_][A-Za-z0-9_.]*"
syn match   viperNumber     "\<\d[0-9_]*\(\.[0-9_]\+\)\=\([eE][+-]\=[0-9_]\+\)\=\>"
syn match   viperNumber     "\<0[xX][0-9A-Fa-f_]\+\>"
syn match   viperFunction   "\(\<def\s\+\)\@<=[A-Za-z_][A-Za-z0-9_]*"
syn match   viperTypeName   "\<[A-Z][A-Za-z0-9_]*\>"
syn match   viperComment    "#.*$" contains=@Spell

syn region  viperDocString  start=+"""+ end=+"""+ contains=@Spell
syn region  viperString     start=+[fFrR]\="+ skip=+\\\\\|\\"+ end=+"+ oneline
syn region  viperString     start=+[fFrR]\='+ skip=+\\\\\|\\'+ end=+'+ oneline

hi def link viperStatement   Statement
hi def link viperConditional Conditional
hi def link viperRepeat      Repeat
hi def link viperOperator    Operator
hi def link viperInclude     Include
hi def link viperStorage     StorageClass
hi def link viperConstant    Constant
hi def link viperSelf        Identifier
hi def link viperType        Type
hi def link viperTypeName    Type
hi def link viperBuiltin     Function
hi def link viperAnnotation  PreProc
hi def link viperNumber      Number
hi def link viperFunction    Function
hi def link viperComment     Comment
hi def link viperDocString   String
hi def link viperString      String

let b:current_syntax = "viper"
