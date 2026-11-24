#include "tree_sitter/parser.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// Keep this order in sync with the `externals` array in grammar.js.
enum TokenType {
  NEWLINE,
  BLOCK_LBRACE,
  LIST_LBRACE,
  MAP_LBRACE,
  RBRACE,
  STRING_START,
  STRING_END,
  INTERPOLATION_START,
  INTERPOLATION_END,
  STRING_CONTENT,
  ESCAPE_SEQUENCE,
};

#define MARTITE_SCANNER_MAX_DEPTH 128

enum Frame {
  FRAME_BLOCK = 1,
  FRAME_LIST = 2,
  FRAME_STRING = 5,
  FRAME_INTERPOLATION = 6,
};

typedef struct {
  uint8_t depth;
  uint8_t frames[MARTITE_SCANNER_MAX_DEPTH];
} Scanner;

static inline uint8_t scanner_top(const Scanner *scanner) {
  return scanner->depth == 0 ? 0 : scanner->frames[scanner->depth - 1];
}

static void scanner_push(Scanner *scanner, uint8_t frame) {
  if (scanner->depth < MARTITE_SCANNER_MAX_DEPTH) {
    scanner->frames[scanner->depth++] = frame;
  } else {
    // Deeply nested input is the compiler's E002 job, not the editor's.
    // Drop the stack rather than overflowing it; recovery starts fresh.
    scanner->depth = 0;
  }
}

static void scanner_pop_until(Scanner *scanner, uint8_t frame) {
  while (scanner->depth > 0) {
    uint8_t top = scanner->frames[--scanner->depth];
    if (top == frame) {
      return;
    }
  }
}

static bool scanner_eof(const TSLexer *lexer) {
  return lexer->eof(lexer);
}

static void advance(TSLexer *lexer) {
  lexer->advance(lexer, false);
}

// A newline is significant only in normal code or directly in a block. Inside
// parentheses, brackets, list-style braces, strings, and interpolation
// expressions it is trivia (section 1: the innermost bracket wins).
static bool newline_is_significant(const Scanner *scanner) {
  uint8_t top = scanner_top(scanner);
  return top == 0 || top == FRAME_BLOCK;
}

// Look past whitespace, blank lines, and comments for a token that cannot
// start a statement (section 1). Such a token makes the preceding newline
// trivia.
static bool newline_is_continuation(TSLexer *lexer) {
  for (;;) {
    if (scanner_eof(lexer)) {
      return false;
    }

    int32_t c = lexer->lookahead;
    if (c == ' ' || c == '\t' || c == '\r' || c == '\n') {
      advance(lexer);
      continue;
    }

    if (c == '/') {
      advance(lexer);
      if (lexer->lookahead != '/') {
        return false;
      }
      while (!scanner_eof(lexer) && lexer->lookahead != '\n') {
        advance(lexer);
      }
      continue;
    }

    break;
  }

  int32_t c = lexer->lookahead;

  if (c == '.') {
    return true;
  }

  if (c == '?') {
    advance(lexer);
    return lexer->lookahead == '?';
  }

  if ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '_') {
    char word[8];
    size_t len = 0;
    while (!scanner_eof(lexer) &&
           ((lexer->lookahead >= 'a' && lexer->lookahead <= 'z') ||
            (lexer->lookahead >= 'A' && lexer->lookahead <= 'Z') ||
            (lexer->lookahead >= '0' && lexer->lookahead <= '9') ||
            lexer->lookahead == '_')) {
      if (len + 1 < sizeof(word)) {
        word[len] = (char)lexer->lookahead;
      }
      len++;
      advance(lexer);
    }
    if (len >= sizeof(word)) {
      return false;
    }
    word[len < sizeof(word) ? len : sizeof(word) - 1] = '\0';

    if (strcmp(word, "else") == 0 || strcmp(word, "and") == 0 ||
        strcmp(word, "or") == 0) {
      return true;
    }
  }

  return false;
}

static bool scan_newline(TSLexer *lexer) {
  if (lexer->lookahead == '\r') {
    advance(lexer);
    if (lexer->lookahead == '\n') {
      advance(lexer);
    }
  } else {
    advance(lexer);
  }

  // The token is exactly the physical newline; any further movement is only
  // lookahead. `mark_end` keeps the parser's token range correct.
  lexer->mark_end(lexer);

  if (newline_is_continuation(lexer)) {
    return false;
  }

  lexer->result_symbol = NEWLINE;
  return true;
}

static bool scan_string_content(TSLexer *lexer) {
  bool consumed = false;
  for (;;) {
    if (scanner_eof(lexer)) {
      break;
    }
    int32_t c = lexer->lookahead;
    if (c == '"' || c == '{' || c == '\\' || c == '\n' || c == '\r') {
      break;
    }
    advance(lexer);
    consumed = true;
  }

  if (!consumed) {
    return false;
  }

  lexer->mark_end(lexer);
  lexer->result_symbol = STRING_CONTENT;
  return true;
}

static bool scan_escape(TSLexer *lexer) {
  advance(lexer); // backslash

  if (scanner_eof(lexer)) {
    lexer->mark_end(lexer);
    lexer->result_symbol = ESCAPE_SEQUENCE;
    return true;
  }

  if (lexer->lookahead == 'u') {
    advance(lexer);
    if (lexer->lookahead == '{') {
      advance(lexer);
      while (!scanner_eof(lexer) && lexer->lookahead != '}' && lexer->lookahead != '\n') {
        advance(lexer);
      }
      if (lexer->lookahead == '}') {
        advance(lexer);
      }
    }
  } else {
    advance(lexer);
  }

  lexer->mark_end(lexer);
  lexer->result_symbol = ESCAPE_SEQUENCE;
  return true;
}

void *tree_sitter_martite_external_scanner_create(void) {
  Scanner *scanner = (Scanner *)calloc(1, sizeof(Scanner));
  return scanner;
}

void tree_sitter_martite_external_scanner_destroy(void *payload) {
  free(payload);
}

unsigned tree_sitter_martite_external_scanner_serialize(void *payload, char *buffer) {
  Scanner *scanner = (Scanner *)payload;
  unsigned length = 1u + scanner->depth;
  if (length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE) {
    return 0;
  }
  buffer[0] = (char)scanner->depth;
  if (scanner->depth > 0) {
    memcpy(buffer + 1, scanner->frames, scanner->depth);
  }
  return length;
}

void tree_sitter_martite_external_scanner_deserialize(
    void *payload,
    const char *buffer,
    unsigned length) {
  Scanner *scanner = (Scanner *)payload;
  memset(scanner, 0, sizeof(Scanner));

  if (length == 0) {
    return;
  }

  uint8_t depth = (uint8_t)buffer[0];
  if (depth > MARTITE_SCANNER_MAX_DEPTH) {
    depth = MARTITE_SCANNER_MAX_DEPTH;
  }
  if (length < 1u + depth) {
    return;
  }

  scanner->depth = depth;
  if (depth > 0) {
    memcpy(scanner->frames, buffer + 1, depth);
  }
}

bool tree_sitter_martite_external_scanner_scan(
    void *payload,
    TSLexer *lexer,
    const bool *valid_symbols) {
  Scanner *scanner = (Scanner *)payload;
  uint8_t top = scanner_top(scanner);

  if (top != FRAME_STRING) {
    for (;;) {
      while (!scanner_eof(lexer) &&
             (lexer->lookahead == ' ' || lexer->lookahead == '\t')) {
        lexer->advance(lexer, true);
      }

      if (scanner_eof(lexer)) {
        break;
      }

      if (lexer->lookahead == '\n' || lexer->lookahead == '\r') {
        if (valid_symbols[NEWLINE] && newline_is_significant(scanner)) {
          if (scan_newline(lexer)) {
            return true;
          }
          // Returning false makes the parser reset to the start of the
          // newline; the whitespace extra skips it and the continuation is
          // reparsed.
          return false;
        }

        // The newline is trivia in this context. Skip it here rather than
        // returning false, because the lexer does not call the external
        // scanner again after consuming an extra.
        if (lexer->lookahead == '\r') {
          lexer->advance(lexer, true);
          if (lexer->lookahead == '\n') {
            lexer->advance(lexer, true);
          }
        } else {
          lexer->advance(lexer, true);
        }
        continue;
      }

      if (lexer->lookahead == '/') {
        lexer->advance(lexer, true);
        if (lexer->lookahead != '/') {
          // Division, not a comment. Let the internal lexer handle it.
          return false;
        }
        while (!scanner_eof(lexer) && lexer->lookahead != '\n') {
          lexer->advance(lexer, true);
        }
        continue;
      }

      break;
    }
  }

  if (top == FRAME_INTERPOLATION &&
      valid_symbols[INTERPOLATION_END] &&
      (scanner_eof(lexer) || lexer->lookahead == '\n' || lexer->lookahead == '\r')) {
    lexer->mark_end(lexer);
    scanner_pop_until(scanner, FRAME_INTERPOLATION);
    lexer->result_symbol = INTERPOLATION_END;
    return true;
  }

  if (top == FRAME_INTERPOLATION &&
      valid_symbols[INTERPOLATION_END] && lexer->lookahead == '}') {
    advance(lexer);
    lexer->mark_end(lexer);
    scanner_pop_until(scanner, FRAME_INTERPOLATION);
    lexer->result_symbol = INTERPOLATION_END;
    return true;
  }

  if (top != FRAME_STRING) {
    if (lexer->lookahead == '{' && valid_symbols[BLOCK_LBRACE]) {
      advance(lexer);
      lexer->mark_end(lexer);
      scanner_push(scanner, FRAME_BLOCK);
      lexer->result_symbol = BLOCK_LBRACE;
      return true;
    }

    if (lexer->lookahead == '{' && valid_symbols[LIST_LBRACE]) {
      advance(lexer);
      lexer->mark_end(lexer);
      scanner_push(scanner, FRAME_LIST);
      lexer->result_symbol = LIST_LBRACE;
      return true;
    }

    if (lexer->lookahead == '{' && valid_symbols[MAP_LBRACE]) {
      advance(lexer);
      lexer->mark_end(lexer);
      scanner_push(scanner, FRAME_LIST);
      lexer->result_symbol = MAP_LBRACE;
      return true;
    }

    if (lexer->lookahead == '}' && valid_symbols[RBRACE]) {
      advance(lexer);
      lexer->mark_end(lexer);
      while (scanner->depth > 0) {
        uint8_t frame = scanner->frames[--scanner->depth];
        if (frame == FRAME_BLOCK || frame == FRAME_LIST) {
          break;
        }
      }
      lexer->result_symbol = RBRACE;
      return true;
    }
  }

  if (top == FRAME_STRING) {
    if (!valid_symbols[STRING_CONTENT] && !valid_symbols[STRING_END] &&
        !valid_symbols[INTERPOLATION_START] && !valid_symbols[ESCAPE_SEQUENCE]) {
      // The parser no longer expects string tokens (typically after an edit).
      // Drop the string state so the next scan can recover in normal mode.
      scanner_pop_until(scanner, FRAME_STRING);
      return false;
    }

    // A raw newline or EOF inside a string means the closing quote is missing.
    // Close the string with a zero-width token so the following statement can
    // recover instead of being swallowed as string content.
    if ((scanner_eof(lexer) || lexer->lookahead == '\n' || lexer->lookahead == '\r') &&
        valid_symbols[STRING_END]) {
      lexer->mark_end(lexer);
      scanner_pop_until(scanner, FRAME_STRING);
      lexer->result_symbol = STRING_END;
      return true;
    }

    if (valid_symbols[STRING_END] && lexer->lookahead == '"') {
      advance(lexer);
      lexer->mark_end(lexer);
      scanner_pop_until(scanner, FRAME_STRING);
      lexer->result_symbol = STRING_END;
      return true;
    }

    if (valid_symbols[INTERPOLATION_START] && lexer->lookahead == '{') {
      advance(lexer);
      lexer->mark_end(lexer);
      scanner_push(scanner, FRAME_INTERPOLATION);
      lexer->result_symbol = INTERPOLATION_START;
      return true;
    }

    if (valid_symbols[ESCAPE_SEQUENCE] && lexer->lookahead == '\\') {
      return scan_escape(lexer);
    }

    if (valid_symbols[STRING_CONTENT]) {
      return scan_string_content(lexer);
    }

    return false;
  }

  if (top == FRAME_INTERPOLATION) {
    if (valid_symbols[INTERPOLATION_END] && lexer->lookahead == '}') {
      advance(lexer);
      lexer->mark_end(lexer);
      scanner_pop_until(scanner, FRAME_INTERPOLATION);
      lexer->result_symbol = INTERPOLATION_END;
      return true;
    }
  }

  if (valid_symbols[STRING_START] && lexer->lookahead == '"') {
    advance(lexer);
    lexer->mark_end(lexer);
    scanner_push(scanner, FRAME_STRING);
    lexer->result_symbol = STRING_START;
    return true;
  }

  return false;
}
