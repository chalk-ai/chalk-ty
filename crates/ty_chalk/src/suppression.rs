use ruff_db::files::File;
use ruff_db::parsed::parsed_module;
use ruff_db::source::source_text;
use ruff_python_ast::token::TokenKind;
use ruff_python_ast::visitor::source_order::{SourceOrderVisitor, walk_stmt};
use ruff_python_ast::{self as ast};
use ruff_text_size::{Ranged, TextRange, TextSize};
use ty_python_semantic::chalk::Definition;
use ty_python_semantic::{Db, HasDefinition, SemanticModel};

#[derive(Clone, Copy, Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) enum SuppressionCode {
    UnsupportedFunction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FunctionSuppression<'db> {
    pub(crate) definition: Definition<'db>,
    pub(crate) function: TextRange,
    pub(crate) directive: TextRange,
    pub(crate) code: SuppressionCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StatementSuppression {
    pub(crate) statement: TextRange,
    pub(crate) directive: TextRange,
    pub(crate) code: SuppressionCode,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub enum InvalidSuppressionReason {
    ExpectedIgnore,
    Blanket,
    ExpectedCodeList,
    MissingClosingBracket,
    EmptyCodeList,
    MalformedCodeList,
    TrailingContent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) enum SuppressionProblemKind {
    Invalid(InvalidSuppressionReason),
    UnknownCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct SuppressionProblem {
    pub(crate) range: TextRange,
    pub(crate) kind: SuppressionProblemKind,
}

#[derive(Debug)]
pub(crate) struct Suppressions<'db> {
    function: Box<[FunctionSuppression<'db>]>,
    statement: Box<[StatementSuppression]>,
    problems: Box<[SuppressionProblem]>,
}

impl<'db> Suppressions<'db> {
    pub(crate) fn function(&self) -> &[FunctionSuppression<'db>] {
        &self.function
    }

    pub(crate) fn statement(&self) -> &[StatementSuppression] {
        &self.statement
    }

    pub(crate) fn problems(&self) -> &[SuppressionProblem] {
        &self.problems
    }

    #[cfg(test)]
    fn suppresses_function(&self, definition: Definition<'db>, code: SuppressionCode) -> bool {
        self.function
            .iter()
            .any(|suppression| suppression.definition == definition && suppression.code == code)
    }

    #[cfg(test)]
    fn suppresses_function_range(&self, function: TextRange, code: SuppressionCode) -> bool {
        self.function
            .iter()
            .any(|suppression| suppression.function == function && suppression.code == code)
    }

    #[cfg(test)]
    fn suppresses_statement(&self, statement: TextRange, code: SuppressionCode) -> bool {
        self.statement
            .iter()
            .any(|suppression| suppression.statement == statement && suppression.code == code)
    }
}

pub(crate) fn extract_suppressions(db: &dyn Db, file: File) -> Suppressions<'_> {
    let source = source_text(db, file);
    let parsed = parsed_module(db, file).load(db);
    let lines = Lines::new(&source);
    let mut directives = Vec::new();
    let mut problems = Vec::new();

    for token in parsed
        .tokens()
        .iter()
        .filter(|token| token.kind() == TokenKind::Comment)
    {
        if let Some(directive) = parse_comment(&source, token.range(), &lines, &mut problems) {
            directives.push(directive);
        }
    }

    let model = SemanticModel::new(db, file);
    let mut visitor = SuppressionVisitor {
        model: &model,
        lines: &lines,
        directives: &directives,
        function: Vec::new(),
        statement: Vec::new(),
    };
    visitor.visit_body(&parsed.syntax().body);

    Suppressions {
        function: visitor.function.into_boxed_slice(),
        statement: visitor.statement.into_boxed_slice(),
        problems: problems.into_boxed_slice(),
    }
}

#[derive(Clone, Copy, Debug)]
struct ParsedDirective {
    range: TextRange,
    line: usize,
    own_line: bool,
    suppresses_unsupported_function: bool,
}

struct SuppressionVisitor<'a, 'db> {
    model: &'a SemanticModel<'db>,
    lines: &'a Lines,
    directives: &'a [ParsedDirective],
    function: Vec<FunctionSuppression<'db>>,
    statement: Vec<StatementSuppression>,
}

impl SourceOrderVisitor<'_> for SuppressionVisitor<'_, '_> {
    fn visit_stmt(&mut self, statement: &ast::Stmt) {
        let range = statement.range();
        let start_line = self.lines.line(range.start());
        let end_line = self.lines.line(range.end() - TextSize::new(1));

        for directive in self.directives.iter().filter(|directive| {
            directive.suppresses_unsupported_function
                && !directive.own_line
                && matches!(directive.line, line if line == start_line || line == end_line)
        }) {
            if let Some(suppression) = self
                .statement
                .iter_mut()
                .find(|suppression| suppression.directive == directive.range)
            {
                if range.start() > suppression.statement.start()
                    || (range.start() == suppression.statement.start()
                        && range.end() < suppression.statement.end())
                {
                    suppression.statement = range;
                }
            } else {
                self.statement.push(StatementSuppression {
                    statement: range,
                    directive: directive.range,
                    code: SuppressionCode::UnsupportedFunction,
                });
            }
        }

        if let ast::Stmt::FunctionDef(function) = statement {
            let anchor = function
                .decorator_list
                .first()
                .map_or_else(|| function.start(), Ranged::start);
            let anchor_line = self.lines.line(anchor);
            if let Some(previous_line) = anchor_line.checked_sub(1)
                && let Some(directive) = self.directives.iter().find(|directive| {
                    directive.line == previous_line
                        && directive.own_line
                        && directive.suppresses_unsupported_function
                })
            {
                self.function.push(FunctionSuppression {
                    definition: function.definition(self.model),
                    function: function.range,
                    directive: directive.range,
                    code: SuppressionCode::UnsupportedFunction,
                });
            }
        }

        walk_stmt(self, statement);
    }
}

fn parse_comment(
    source: &str,
    range: TextRange,
    lines: &Lines,
    problems: &mut Vec<SuppressionProblem>,
) -> Option<ParsedDirective> {
    let start = usize::from(range.start());
    let text = &source[start..usize::from(range.end())];
    let bytes = text.as_bytes();
    let mut cursor = 1;
    skip_whitespace(bytes, &mut cursor);

    if !bytes[cursor..].starts_with(b"chalk:") {
        return None;
    }
    cursor += "chalk:".len();
    skip_whitespace(bytes, &mut cursor);

    let ignore_start = cursor;
    if !bytes[cursor..].starts_with(b"ignore") {
        let end = trimmed_end(bytes, cursor);
        problems.push(SuppressionProblem {
            range: absolute_range(start, cursor, end),
            kind: SuppressionProblemKind::Invalid(InvalidSuppressionReason::ExpectedIgnore),
        });
        return None;
    }
    cursor += "ignore".len();

    if bytes
        .get(cursor)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
    {
        let end = trimmed_end(bytes, cursor);
        problems.push(SuppressionProblem {
            range: absolute_range(start, cursor, end),
            kind: SuppressionProblemKind::Invalid(InvalidSuppressionReason::ExpectedCodeList),
        });
        return None;
    }

    skip_whitespace(bytes, &mut cursor);
    if cursor == bytes.len() {
        problems.push(SuppressionProblem {
            range: absolute_range(start, ignore_start, ignore_start + "ignore".len()),
            kind: SuppressionProblemKind::Invalid(InvalidSuppressionReason::Blanket),
        });
        return None;
    }
    if bytes[cursor] != b'[' {
        let end = trimmed_end(bytes, cursor);
        problems.push(SuppressionProblem {
            range: absolute_range(start, cursor, end),
            kind: SuppressionProblemKind::Invalid(InvalidSuppressionReason::ExpectedCodeList),
        });
        return None;
    }

    let open = cursor;
    cursor += 1;
    let Some(relative_close) = bytes[cursor..].iter().position(|byte| *byte == b']') else {
        problems.push(SuppressionProblem {
            range: absolute_range(start, open, bytes.len()),
            kind: SuppressionProblemKind::Invalid(InvalidSuppressionReason::MissingClosingBracket),
        });
        return None;
    };
    let close = cursor + relative_close;
    let mut trailing = close + 1;
    skip_whitespace(bytes, &mut trailing);
    if trailing != bytes.len() {
        problems.push(SuppressionProblem {
            range: absolute_range(start, trailing, trimmed_end(bytes, trailing)),
            kind: SuppressionProblemKind::Invalid(InvalidSuppressionReason::TrailingContent),
        });
        return None;
    }

    let (contents_start, contents_end) = trim(bytes, cursor, close);
    if contents_start == contents_end {
        problems.push(SuppressionProblem {
            range: absolute_range(start, open, close + 1),
            kind: SuppressionProblemKind::Invalid(InvalidSuppressionReason::EmptyCodeList),
        });
        return None;
    }

    let mut tokens = Vec::new();
    let mut token_start = cursor;
    loop {
        let token_end = bytes[token_start..close]
            .iter()
            .position(|byte| *byte == b',')
            .map_or(close, |offset| token_start + offset);
        let (trimmed_start, trimmed_end) = trim(bytes, token_start, token_end);
        if trimmed_start == trimmed_end {
            let (invalid_start, invalid_end) = if token_end < close {
                (token_end, token_end + 1)
            } else if token_start > cursor {
                (token_start - 1, token_start)
            } else {
                (trimmed_start, trimmed_end)
            };
            problems.push(SuppressionProblem {
                range: absolute_range(start, invalid_start, invalid_end),
                kind: SuppressionProblemKind::Invalid(InvalidSuppressionReason::MalformedCodeList),
            });
            return None;
        }
        if !bytes[trimmed_start..trimmed_end]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            problems.push(SuppressionProblem {
                range: absolute_range(start, trimmed_start, trimmed_end),
                kind: SuppressionProblemKind::Invalid(InvalidSuppressionReason::MalformedCodeList),
            });
            return None;
        }
        tokens.push((trimmed_start, trimmed_end));

        if token_end == close {
            break;
        }
        token_start = token_end + 1;
    }

    let mut suppresses_unsupported_function = false;
    for (token_start, token_end) in tokens {
        if &text[token_start..token_end] == "unsupported-function" {
            suppresses_unsupported_function = true;
        } else {
            problems.push(SuppressionProblem {
                range: absolute_range(start, token_start, token_end),
                kind: SuppressionProblemKind::UnknownCode,
            });
        }
    }

    let line = lines.line(range.start());
    let line_start = lines.start(line);
    let own_line = source[usize::from(line_start)..start].trim().is_empty();
    Some(ParsedDirective {
        range,
        line,
        own_line,
        suppresses_unsupported_function,
    })
}

fn skip_whitespace(bytes: &[u8], cursor: &mut usize) {
    while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
        *cursor += 1;
    }
}

fn trimmed_end(bytes: &[u8], start: usize) -> usize {
    let mut end = bytes.len();
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    end
}

fn trim(bytes: &[u8], mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start, end)
}

fn absolute_range(base: usize, start: usize, end: usize) -> TextRange {
    TextRange::new(
        TextSize::try_from(base + start).expect("source offset fits in TextSize"),
        TextSize::try_from(base + end).expect("source offset fits in TextSize"),
    )
}

struct Lines {
    starts: Vec<TextSize>,
}

impl Lines {
    fn new(source: &str) -> Self {
        let mut starts = vec![TextSize::default()];
        starts.extend(source.match_indices('\n').map(|(index, _)| {
            TextSize::try_from(index + 1).expect("source offset fits in TextSize")
        }));
        Self { starts }
    }

    fn line(&self, offset: TextSize) -> usize {
        self.starts.partition_point(|start| *start <= offset) - 1
    }

    fn start(&self, line: usize) -> TextSize {
        self.starts[line]
    }
}
