// The external scanner: NEWLINE, INDENT and DEDENT, computed the way the compiler's lexer does
// (docs/03-compiler-architecture.md, "Lexer"). Inside brackets the grammar never asks for a
// NEWLINE, so line breaks there are plain whitespace. The tokens are zero-width: the scanner only
// looks ahead over spaces, line breaks and comments, and the grammar's own lexer then reads the
// comments as nodes.

#include "tree_sitter/parser.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

enum TokenType { NEWLINE, INDENT, DEDENT };

#define MAX_DEPTH 256

typedef struct {
    // indents[0] is always 0; each open block adds its indentation width.
    uint16_t indents[MAX_DEPTH];
    uint16_t depth;
} Scanner;

void *tree_sitter_weby_external_scanner_create(void) {
    Scanner *scanner = calloc(1, sizeof(Scanner));
    scanner->depth = 1;
    return scanner;
}

void tree_sitter_weby_external_scanner_destroy(void *payload) { free(payload); }

unsigned tree_sitter_weby_external_scanner_serialize(void *payload, char *buffer) {
    Scanner *scanner = payload;
    // The first entry is always 0, so it is not stored.
    unsigned count = scanner->depth - 1;
    unsigned bytes = count * sizeof(uint16_t);
    if (bytes > TREE_SITTER_SERIALIZATION_BUFFER_SIZE) {
        count = TREE_SITTER_SERIALIZATION_BUFFER_SIZE / sizeof(uint16_t);
        bytes = count * sizeof(uint16_t);
    }
    memcpy(buffer, &scanner->indents[1], bytes);
    return bytes;
}

void tree_sitter_weby_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    Scanner *scanner = payload;
    scanner->indents[0] = 0;
    unsigned count = length / sizeof(uint16_t);
    if (count > MAX_DEPTH - 1) {
        count = MAX_DEPTH - 1;
    }
    memcpy(&scanner->indents[1], buffer, count * sizeof(uint16_t));
    scanner->depth = (uint16_t)(count + 1);
}

static void skip(TSLexer *lexer) { lexer->advance(lexer, true); }

bool tree_sitter_weby_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    Scanner *scanner = payload;
    // During error recovery every symbol is valid; inventing layout then only makes it worse.
    bool error_recovery = valid_symbols[NEWLINE] && valid_symbols[INDENT] && valid_symbols[DEDENT];
    if (!valid_symbols[NEWLINE] && !valid_symbols[INDENT] && !valid_symbols[DEDENT]) {
        return false;
    }

    lexer->mark_end(lexer);

    bool at_line_end = false;
    uint32_t width = 0;
    // The indentation of the first comment line, which belongs to the block it is indented into.
    int32_t comment_width = -1;
    for (;;) {
        if (lexer->lookahead == '\n') {
            at_line_end = true;
            width = 0;
            skip(lexer);
        } else if (lexer->lookahead == ' ') {
            width++;
            skip(lexer);
        } else if (lexer->lookahead == '\t') {
            // The compiler rejects tabs; count one as a single column so that editing goes on.
            width++;
            skip(lexer);
        } else if (lexer->lookahead == '\r' || lexer->lookahead == '\f') {
            width = 0;
            skip(lexer);
        } else if (lexer->lookahead == '#') {
            if (comment_width == -1) {
                comment_width = (int32_t)width;
            }
            while (lexer->lookahead && lexer->lookahead != '\n' && !lexer->eof(lexer)) {
                skip(lexer);
            }
        } else if (lexer->eof(lexer)) {
            at_line_end = true;
            width = 0;
            break;
        } else {
            break;
        }
    }

    if (!at_line_end) {
        return false;
    }
    uint16_t current = scanner->indents[scanner->depth - 1];

    if (valid_symbols[INDENT] && width > current && !error_recovery) {
        if (scanner->depth < MAX_DEPTH) {
            scanner->indents[scanner->depth++] = (uint16_t)width;
        }
        lexer->result_symbol = INDENT;
        return true;
    }

    if (valid_symbols[DEDENT] && width < current && scanner->depth > 1 && comment_width < (int32_t)current) {
        scanner->depth--;
        lexer->result_symbol = DEDENT;
        return true;
    }

    if (valid_symbols[NEWLINE] && !error_recovery) {
        lexer->result_symbol = NEWLINE;
        return true;
    }

    return false;
}
