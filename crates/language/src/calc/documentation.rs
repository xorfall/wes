//! Semantic help for the closed operation vocabulary; registered spelling/arity stays in Package.
use super::Operation;

pub struct OperationHelp {
    pub parameters: &'static [(&'static str, &'static str)],
    pub returns: &'static str,
    pub summary: &'static str,
    pub notes: &'static str,
    /// Expression for the standard package. Context-dependent examples state their prerequisites.
    pub example: &'static str,
    pub prerequisites: &'static str,
}
impl Operation {
    pub fn help(self) -> OperationHelp {
        use Operation::*;
        let (parameters, returns, summary, example): (
            &'static [(&'static str, &'static str)],
            _,
            _,
            _,
        ) = match self {
            RegexTest => (
                &[("source", "Text"), ("pattern", "Text")],
                "Bool",
                "Test a bounded regex without collecting matches or capture groups.",
                "regexTest('status=503','status=[45][0-9]{2}')",
            ),
            StripAnsi => (
                &[("source", "Text")],
                "{text:Text,spans:List<Record>}",
                "Remove explicit terminal escapes and retain mappings to original bytes.",
                "stripAnsi('plain text').text",
            ),
            Instant => (
                &[("text", "Text")],
                "Instant",
                "Parse an RFC3339 timestamp with an explicit offset.",
                "instant('2025-01-01T00:00:00Z')",
            ),
            Duration => (
                &[("text", "Text")],
                "Duration",
                "Parse an ISO8601 duration.",
                "duration('PT5S')",
            ),
            Interval => (
                &[("start", "Instant"), ("end", "Instant")],
                "Interval",
                "Construct a validated half-open interval [start,end).",
                "interval(instant('2025-01-01T00:00:00Z'),instant('2025-01-01T00:01:00Z'))",
            ),
            Around => (
                &[("center", "Instant"), ("radius", "Duration")],
                "Interval",
                "Construct an interval around an instant with a nonnegative radius.",
                "around(instant('2025-01-01T00:00:00Z'),duration('PT5S'))",
            ),
            FromEpochSeconds => (
                &[("number", "Int | Decimal")],
                "Instant",
                "Convert epoch seconds to an instant.",
                "fromEpochSeconds(0)",
            ),
            FromEpochMillis => (
                &[("number", "Int | Decimal")],
                "Instant",
                "Convert epoch milliseconds to an instant.",
                "fromEpochMillis(1000)",
            ),
            FromEpochNanos => (
                &[("number", "Int | Decimal")],
                "Instant",
                "Convert epoch nanoseconds to an instant.",
                "fromEpochNanos(1)",
            ),
            ToEpochSeconds => (
                &[("instant", "Instant")],
                "Decimal",
                "Return exact epoch seconds.",
                "toEpochSeconds(instant('1970-01-01T00:00:01Z'))",
            ),
            ToEpochMillis => (
                &[("instant", "Instant")],
                "Decimal",
                "Return exact epoch milliseconds.",
                "toEpochMillis(instant('1970-01-01T00:00:01Z'))",
            ),
            ToEpochNanos => (
                &[("instant", "Instant")],
                "Decimal",
                "Return exact epoch nanoseconds.",
                "text(toEpochNanos(instant('1970-01-01T00:00:01Z')))",
            ),
            DurationSeconds => (
                &[("number", "Int | Decimal")],
                "Duration",
                "Construct a duration from seconds.",
                "durationSeconds(2)",
            ),
            DurationMillis => (
                &[("number", "Int | Decimal")],
                "Duration",
                "Construct a duration from milliseconds.",
                "durationMillis(500)",
            ),
            DurationNanos => (
                &[("number", "Int | Decimal")],
                "Duration",
                "Construct a duration from nanoseconds.",
                "durationNanos(1)",
            ),
            ToSeconds => (
                &[("duration", "Duration")],
                "Decimal",
                "Return exact duration seconds.",
                "toSeconds(duration('PT2S'))",
            ),
            ToMillis => (
                &[("duration", "Duration")],
                "Decimal",
                "Return exact duration milliseconds.",
                "toMillis(duration('PT2S'))",
            ),
            ToNanos => (
                &[("duration", "Duration")],
                "Decimal",
                "Return exact duration nanoseconds.",
                "text(toNanos(duration('PT2S')))",
            ),
            UtcParts => (
                &[("instant", "Instant")],
                "Record",
                "Return UTC calendar fields.",
                "utcParts(instant('2025-01-01T00:00:00Z'))",
            ),
            Some => (
                &[("value", "T")],
                "Option<T>",
                "Wrap a present value.",
                "some(42)",
            ),
            IsSome => (
                &[("option", "Option<T>")],
                "Bool",
                "Test whether an option contains a value.",
                "isSome(some(42))",
            ),
            UnwrapOr => (
                &[("option", "Option<T>"), ("fallback", "T")],
                "T",
                "Return a present value or the fallback.",
                "unwrapOr(none,42)",
            ),
            Has => (
                &[("record", "Record"), ("key", "Text")],
                "Bool",
                "Test for a record field.",
                "has({status:200},'status')",
            ),
            Length => (
                &[("value", "Text | Bytes | List<T> | Record")],
                "Int",
                "Return character, byte, item or field count; not available on Iter.",
                "length('a😀b')",
            ),
            Keys => (
                &[("record", "Record")],
                "List<Text>",
                "Return record field names.",
                "keys({status:200})",
            ),
            Map => (
                &[("source", "List<T> | Iter<T>"), ("callback", "(T) -> U")],
                "List<U> | Iter<U>",
                "Transform each item; preserve the source collection kind.",
                "collect(map(iter.items([1,2]),x => x*2))",
            ),
            Filter => (
                &[
                    ("source", "List<T> | Iter<T>"),
                    ("predicate", "(T) -> Bool"),
                ],
                "List<T> | Iter<T>",
                "Keep items whose predicate is true.",
                "filter([1,2,3],x => x > 1)",
            ),
            Reduce => (
                &[
                    ("source", "List<T> | Iter<T>"),
                    ("callback", "(A,T) -> A"),
                    ("initial", "A"),
                ],
                "A",
                "Fold items into an accumulator in order.",
                "reduce([1,2,3],(a,x) => a+x,0)",
            ),
            Join => (
                &[("items", "List<Text>"), ("separator", "Text")],
                "Text",
                "Join text items without a trailing separator.",
                "join(['a','b'],',')",
            ),
            WithFields => (
                &[("record", "Record"), ("patch", "Record")],
                "Record",
                "Return a fresh shallow record with replaced/added fields.",
                "withFields({status:200},{status:503})",
            ),
            Slice => (
                &[
                    ("source", "Text | List<T>"),
                    ("start", "Int"),
                    ("end", "Int"),
                ],
                "Text | List<T>",
                "Select [start,end); omitted end is length.",
                "slice('a😀b',1,2)",
            ),
            Concat => (
                &[("left", "List<T>"), ("right", "List<U>")],
                "List<T | U>",
                "Concatenate two finite lists in order.",
                "concat([1,2],[3])",
            ),
            SortBy => (
                &[
                    ("items", "List<T>"),
                    ("key", "(T) -> Int | Decimal | Text | Instant | Duration"),
                ],
                "List<T>",
                "Stable ascending sort using matching key types.",
                "sortBy([{n:2},{n:1}],x => x.n)",
            ),
            Range => (
                &[("endOrStart", "Int"), ("end", "Int"), ("step", "Int")],
                "List<Int>",
                "Build a finite integer range with an exclusive end.",
                "range(0,6,2)",
            ),
            Decimal => (
                &[("value", "Int | Decimal | Text")],
                "Decimal",
                "Convert explicitly to an exact decimal.",
                "decimal('1.25')",
            ),
            Int => (
                &[("value", "Int | Decimal | Text")],
                "Int",
                "Convert explicitly to a signed 64-bit integer.",
                "int('42')",
            ),
            Text => (
                &[(
                    "value",
                    "Int | Decimal | Bool | Text | Bytes | Instant | Duration | Interval",
                )],
                "Text",
                "Convert a scalar to text; Bytes require valid UTF-8.",
                "text(42)",
            ),
            Div => (
                &[("left", "Int"), ("right", "Int")],
                "Int",
                "Integer quotient; division by zero fails.",
                "div(7,2)",
            ),
            Rem => (
                &[("left", "Int"), ("right", "Int")],
                "Int",
                "Integer remainder; division by zero fails.",
                "rem(7,2)",
            ),
            RoundDiv => (
                &[
                    ("left", "Int | Decimal"),
                    ("right", "Int | Decimal"),
                    ("scale", "Int"),
                ],
                "Decimal",
                "Divide with explicit decimal scale (0..1024).",
                "roundDiv(1,3,2)",
            ),
            ParseJson => (
                &[("text", "Text"), ("contract", "Text")],
                "JSON value | named contract",
                "Parse JSON, optionally validating against a named contract.",
                "parseJson('{\"status\":200}')",
            ),
            DecodeJson => (
                &[("input", "Text | Bytes"), ("contract", "Text")],
                "{ok:Bool,value:Option<T>,error:Option<{code:Text,message:Text,line:Option<Int>,column:Option<Int>}>}",
                "Decode JSON as a result value; invalid input produces a bounded diagnostic.",
                "decodeJson('{\"status\":200}').ok",
            ),
            HttpStatus => (
                &[("status", "Int | Text")],
                "HttpStatusDefinition",
                "Look up an HTTP status code (100..599) locally.",
                "httpStatus(503)",
            ),
            HttpError => (
                &[("codeOrError", "Text | Record {code:Text}")],
                "HttpErrorDefinition",
                "Look up a local HTTP adapter error code.",
                "httpError('HTTP003')",
            ),
            HttpCatalogue => (
                &[("catalogue", "Text")],
                "List<HttpStatusDefinition> | List<HttpErrorDefinition>",
                "Read the local statuses or errors catalogue.",
                "httpCatalogue('statuses')",
            ),
            HttpAnalysis => (
                &[("trace", "Record")],
                "HttpAnalysis",
                "Analyse a captured HTTP trace locally; never sends a request.",
                "httpAnalysis($trace)",
            ),
            Check => (
                &[("contract", "Text"), ("value", "T")],
                "named contract",
                "Validate a value and attach its captured contract.",
                "check('Sample',{status:200})",
            ),
            Call => (
                &[
                    ("provider", "Text"),
                    ("path", "List<Text>"),
                    ("arguments", "Record"),
                ],
                "provider result",
                "Invoke a captured provider operation under current permissions and limits.",
                "call('sample',['read'],{key:'demo'})",
            ),
            IterItems => (
                &[("items", "List<T>")],
                "Iter<T>",
                "Lazily traverse a finite list.",
                "collect(iter.items([1,2]))",
            ),
            IterLines => (
                &[("text", "Text")],
                "Iter<Text>",
                "Lazily traverse text lines.",
                "collect(iter.lines('a\\nb'))",
            ),
            IterChars => (
                &[("text", "Text")],
                "Iter<Text>",
                "Lazily traverse Unicode scalar characters.",
                "collect(iter.chars('a😀'))",
            ),
            IterWords => (
                &[("text", "Text")],
                "Iter<Text>",
                "Lazily traverse whitespace-separated words.",
                "collect(iter.words('a b'))",
            ),
            IterSplit => (
                &[("text", "Text"), ("separator", "Text")],
                "Iter<Text>",
                "Lazily split on a nonempty literal separator.",
                "collect(iter.split('a,b',','))",
            ),
            IterRegexSplit => (
                &[("text", "Text"), ("pattern", "Text")],
                "Iter<Text>",
                "Lazily split on a regular expression.",
                "collect(iter.regexSplit('a,b;c','[,;]'))",
            ),
            IterCaptures => (
                &[("text", "Text"), ("pattern", "Text")],
                "Iter<Record {match:Text, groups:List<Option<Text>>}>",
                "Return a full match and its capture groups for each non-overlapping regex match.",
                "collect(iter.captures('a=12 b=7','([a-z])=([0-9]+)'))",
            ),
            IterMatches => (
                &[("text", "Text"), ("pattern", "Text")],
                "Iter<Text>",
                "Return full matched text, not capture groups, for non-overlapping regex matches.",
                "collect(iter.matches('code=503 ok=200','[0-9]+'))",
            ),
            IterKeys => (
                &[("record", "Record")],
                "Iter<Text>",
                "Lazily traverse record keys.",
                "collect(iter.keys({status:200}))",
            ),
            IterValues => (
                &[("record", "Record")],
                "Iter<T>",
                "Lazily traverse record values; heterogeneous fields have a union item type.",
                "collect(iter.values({a:1,b:2}))",
            ),
            IterEntries => (
                &[("record", "Record")],
                "Iter<Record>",
                "Lazily traverse key/value entries.",
                "collect(iter.entries({status:200}))",
            ),
            IterJsonLines => (
                &[("text", "Text")],
                "Iter<JSON value>",
                "Parse one JSON value per line when consumed.",
                "collect(iter.jsonLines('{\"n\":1}\\n{\"n\":2}'))",
            ),
            IterUse => (
                &[("recipe", "Text"), ("source", "recipe input")],
                "Iter<recipe output>",
                "Apply a named captured iterator recipe.",
                "collect(iter.use('SampleLines','first\\nsecond'))",
            ),
            IterChecked => (
                &[("source", "Iter<T>"), ("contract", "Text")],
                "Iter<named contract>",
                "Validate each item lazily against a captured contract.",
                "collect(iter.checked(iter.items([{status:200}]),'Sample'))",
            ),
            Take => (
                &[("source", "Iter<T>"), ("count", "Int")],
                "Iter<T>",
                "Keep at most count items (nonnegative).",
                "collect(take(iter.items([1,2,3]),2))",
            ),
            Skip => (
                &[("source", "Iter<T>"), ("count", "Int")],
                "Iter<T>",
                "Skip the first count items (nonnegative).",
                "collect(skip(iter.items([1,2,3]),1))",
            ),
            Collect => (
                &[("source", "Iter<T>")],
                "List<T>",
                "Consume a lazy iterator into a finite list under work/memory limits.",
                "collect(iter.items([1,2]))",
            ),
            Count => (
                &[("source", "Iter<T>")],
                "Int",
                "Consume a lazy iterator and count its items.",
                "count(iter.words('one two'))",
            ),
            Field => (
                &[("source", "Iter<Record>"), ("name", "Text")],
                "Iter<field type>",
                "Lazily select a field from each record; missing fields fail when consumed.",
                "collect(field(iter.items([{status:200}]),'status'))",
            ),
        };
        let notes = match self {
            RegexTest => {
                "Unanchored unless the pattern explicitly uses anchors. Empty and zero-width patterns are valid. The entire bounded source is charged before searching; compilation uses a run-local bounded cache. Invalid syntax is CAL016 and work/memory/compiled-size limits are CAL006. Use function syntax, not a Text method."
            }
            StripAnsi => {
                "Accepts 7-bit ESC[ CSI and ESC] OSC terminated by BEL or ESC-backslash. Unsupported or incomplete escapes fail with CAL016; other text and line endings are preserved. Use result.text explicitly. spans contain inputStart/inputEnd/outputStart/outputEnd, half-open UTF-8 byte offsets of unchanged runs. Keep original input for evidence: normalization is not a terminal emulator or a replacement for raw bytes. Function syntax only."
            }
            IterCaptures => {
                "Each item is {match:Text,groups:List<Option<Text>>}. match is the full matched text. The full match is excluded from groups; groups[0] is the first capture group, followed by pattern order (including named groups). Unmatched optional groups are none; present groups are some(Text). Use unwrapOr(item.groups[0],'') explicitly to obtain Text."
            }
            IterEntries => {
                "Each entry is a structural Record {key:Text,value:T}; record order is preserved."
            }
            Map | Filter | Reduce => {
                "List processing is eager and may use effectful callbacks. Iter processing is lazy; callbacks must be statically verified pure and cannot capture mutable outer bindings. Switching an effectful List callback to Iter fails with CAL009; use explicit for-of for effects. Consume with collect/count/reduce or for-of. T/U/A describe input-dependent types, not extra syntax."
            }
            Range => {
                "range(end) starts at 0; range(start,end[,step]) defaults step to 1. Step must be nonzero; direction mismatches produce an empty list."
            }
            Int => {
                "Decimal must be integral: no truncation. Numeric Text must parse as an Int. Invalid numeric Text is CAL016; precision and out-of-range values are CAL005."
            }
            Decimal => {
                "Invalid numeric Text is CAL016. No implicit Int/Decimal mixing is performed."
            }
            RoundDiv => {
                "Scale is explicit decimal places; operands are numeric, not Duration. To round a Duration division to whole nanoseconds, use durationNanos(roundDiv(toNanos(total), count, 0)). No implicit time units or rounding."
            }
            Slice => {
                "Indices must be nonnegative and within length. Text uses Unicode scalar characters, not bytes or grapheme clusters."
            }
            WithFields => {
                "No mutation or recursive merge. A nominal contract must be validated again."
            }
            SortBy => {
                "Keys must all have one comparable type; Int and Decimal do not mix implicitly. The key callback must be pure."
            }
            IterJsonLines => {
                "Every line must be valid JSON, including interior blank lines. To skip blank lines explicitly, use iter.lines(source).filter(line => iter.words(line).count() > 0).map(line => parseJson(line)).collect(). This is not an open stream."
            }
            IterItems | IterLines | IterChars | IterWords | IterSplit | IterRegexSplit
            | IterMatches | IterKeys | IterValues | IterUse | IterChecked | Take | Skip | Field => {
                "Iter is a lazy traversal of bounded materialized data, not Stream. Consume explicitly with collect/count/reduce or for-of. Result item types depend on the input/contract; inference can remain Unknown until consumption."
            }
            Call => {
                "May perform external effects. Provider name and operation path must be literal Text and a literal List<Text>, captured at planning; current environment, effect authority, cancellation and budgets apply. Not allowed in pure lazy callbacks."
            }
            HttpCatalogue => {
                "Use literal Text: statuses or errors. The catalogue is bundled; no network request occurs."
            }
            HttpStatus => {
                "Text must contain exactly three digits. The returned Record contains code, name, class, registration, reference and catalogueVersion."
            }
            ParseJson => {
                "Numbers are exact. Without a contract the result is structurally inferred; no implicit nominal contract is attached. The optional contract name must be literal Text."
            }
            DecodeJson => {
                "The optional contract name is literal Text captured at planning. Success has some(value), including JSON null; failure has no value and a fixed diagnostic that never echoes input. Limits, cancellation and unavailable services still fail the calculation. Pure derivation preserves source policy."
            }
            _ => {
                "No implicit coercion. Existing calculation work, memory and cancellation limits apply."
            }
        };
        let prerequisites = match self {
            HttpAnalysis => {
                "Requires $trace from an HTTP trace snapshot (schema:1, profile:http), not a response body."
            }
            Check | IterChecked => "Requires a registered Sample record contract with status:Int.",
            IterUse => {
                "Requires a registered SampleLines iterator recipe with Text input, lines mode and Text output."
            }
            Call => {
                "Requires a configured sample provider with read(key:Text); executing calls the provider under its declared effects."
            }
            _ => "",
        };
        OperationHelp {
            parameters,
            returns,
            summary,
            notes,
            example,
            prerequisites,
        }
    }
}
