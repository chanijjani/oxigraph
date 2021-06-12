use crate::algebra::*;
use crate::query::*;
use crate::term::*;
use crate::update::*;
use oxilangtag::LanguageTag;
use oxiri::{Iri, IriParseError};
use peg::parser;
use peg::str::LineCol;
use rand::random;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::convert::{TryFrom, TryInto};
use std::error::Error;
use std::str::Chars;
use std::str::FromStr;
use std::{char, fmt};

/// Parses a SPARQL query with an optional base IRI to resolve relative IRIs in the query
pub fn parse_query(query: &str, base_iri: Option<&str>) -> Result<Query, ParseError> {
    let mut state = ParserState {
        base_iri: if let Some(base_iri) = base_iri {
            Some(Iri::parse(base_iri.to_owned()).map_err(|e| ParseError {
                inner: ParseErrorKind::InvalidBaseIri(e),
            })?)
        } else {
            None
        },
        namespaces: HashMap::default(),
        used_bnodes: HashSet::default(),
        currently_used_bnodes: HashSet::default(),
        aggregates: Vec::new(),
    };

    parser::QueryUnit(&unescape_unicode_codepoints(query), &mut state).map_err(|e| ParseError {
        inner: ParseErrorKind::Parser(e),
    })
}

/// Parses a SPARQL update with an optional base IRI to resolve relative IRIs in the query
pub fn parse_update(update: &str, base_iri: Option<&str>) -> Result<Update, ParseError> {
    let mut state = ParserState {
        base_iri: if let Some(base_iri) = base_iri {
            Some(Iri::parse(base_iri.to_owned()).map_err(|e| ParseError {
                inner: ParseErrorKind::InvalidBaseIri(e),
            })?)
        } else {
            None
        },
        namespaces: HashMap::default(),
        used_bnodes: HashSet::default(),
        currently_used_bnodes: HashSet::default(),
        aggregates: Vec::new(),
    };

    let operations =
        parser::UpdateInit(&unescape_unicode_codepoints(update), &mut state).map_err(|e| {
            ParseError {
                inner: ParseErrorKind::Parser(e),
            }
        })?;
    Ok(Update {
        operations,
        base_iri: state.base_iri,
    })
}

/// Error returned during SPARQL parsing.
#[derive(Debug)]
pub struct ParseError {
    inner: ParseErrorKind,
}

#[derive(Debug)]
enum ParseErrorKind {
    InvalidBaseIri(IriParseError),
    Parser(peg::error::ParseError<LineCol>),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            ParseErrorKind::InvalidBaseIri(e) => {
                write!(f, "Invalid SPARQL base IRI provided: {}", e)
            }
            ParseErrorKind::Parser(e) => e.fmt(f),
        }
    }
}

impl Error for ParseError {}

struct AnnotatedTerm {
    term: TermPattern,
    annotations: Vec<(NamedNodePattern, Vec<AnnotatedTerm>)>,
}

#[derive(Default)]
struct FocusedTriplePattern<F> {
    focus: F,
    patterns: Vec<TriplePattern>,
}

impl<F> FocusedTriplePattern<F> {
    fn new(focus: F) -> Self {
        Self {
            focus,
            patterns: Vec::new(),
        }
    }
}

impl<F> From<FocusedTriplePattern<F>> for FocusedTriplePattern<Vec<F>> {
    fn from(input: FocusedTriplePattern<F>) -> Self {
        Self {
            focus: vec![input.focus],
            patterns: input.patterns,
        }
    }
}

#[derive(Clone)]
enum VariableOrPropertyPath {
    Variable(Variable),
    PropertyPath(PropertyPathExpression),
}

impl From<Variable> for VariableOrPropertyPath {
    fn from(var: Variable) -> Self {
        VariableOrPropertyPath::Variable(var)
    }
}

impl From<NamedNodePattern> for VariableOrPropertyPath {
    fn from(pattern: NamedNodePattern) -> Self {
        match pattern {
            NamedNodePattern::NamedNode(node) => PropertyPathExpression::from(node).into(),
            NamedNodePattern::Variable(v) => v.into(),
        }
    }
}

impl From<PropertyPathExpression> for VariableOrPropertyPath {
    fn from(path: PropertyPathExpression) -> Self {
        VariableOrPropertyPath::PropertyPath(path)
    }
}

fn add_to_triple_patterns(
    subject: TermPattern,
    predicate: NamedNodePattern,
    object: AnnotatedTerm,
    patterns: &mut Vec<TriplePattern>,
) {
    let triple = TriplePattern::new(subject, predicate, object.term);
    for (p, os) in object.annotations {
        for o in os {
            add_to_triple_patterns(triple.clone().into(), p.clone(), o, patterns)
        }
    }
    patterns.push(triple)
}

fn add_to_triple_or_path_patterns(
    subject: TermPattern,
    predicate: impl Into<VariableOrPropertyPath>,
    object: AnnotatedTermPath,
    patterns: &mut Vec<TripleOrPathPattern>,
) -> Result<(), &'static str> {
    match predicate.into() {
        VariableOrPropertyPath::Variable(p) => {
            add_triple_to_triple_or_path_patterns(subject, p, object, patterns)?;
        }
        VariableOrPropertyPath::PropertyPath(p) => match p {
            PropertyPathExpression::NamedNode(p) => {
                add_triple_to_triple_or_path_patterns(subject, p, object, patterns)?;
            }
            PropertyPathExpression::Reverse(p) => add_to_triple_or_path_patterns(
                object.term,
                *p,
                AnnotatedTermPath {
                    term: subject,
                    annotations: object.annotations,
                },
                patterns,
            )?,
            PropertyPathExpression::Sequence(a, b) => {
                if !object.annotations.is_empty() {
                    return Err("Annotations are not allowed on property paths");
                }
                let middle = bnode();
                add_to_triple_or_path_patterns(
                    subject,
                    *a,
                    AnnotatedTermPath {
                        term: middle.clone().into(),
                        annotations: Vec::new(),
                    },
                    patterns,
                )?;
                add_to_triple_or_path_patterns(
                    middle.into(),
                    *b,
                    AnnotatedTermPath {
                        term: object.term,
                        annotations: Vec::new(),
                    },
                    patterns,
                )?;
            }
            path => {
                if !object.annotations.is_empty() {
                    return Err("Annotations are not allowed on property paths");
                }
                patterns.push(TripleOrPathPattern::Path {
                    subject,
                    path,
                    object: object.term,
                })
            }
        },
    }
    Ok(())
}

fn add_triple_to_triple_or_path_patterns(
    subject: TermPattern,
    predicate: impl Into<NamedNodePattern>,
    object: AnnotatedTermPath,
    patterns: &mut Vec<TripleOrPathPattern>,
) -> Result<(), &'static str> {
    let triple = TriplePattern::new(subject, predicate, object.term);
    for (p, os) in object.annotations {
        for o in os {
            add_to_triple_or_path_patterns(triple.clone().into(), p.clone(), o, patterns)?
        }
    }
    patterns.push(triple.into());
    Ok(())
}

fn build_bgp(patterns: Vec<TripleOrPathPattern>) -> GraphPattern {
    let mut bgp = Vec::with_capacity(patterns.len());
    let mut paths = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        match pattern {
            TripleOrPathPattern::Triple(t) => bgp.push(t),
            TripleOrPathPattern::Path {
                subject,
                path,
                object,
            } => paths.push((subject, path, object)),
        }
    }
    let mut graph_pattern = GraphPattern::Bgp(bgp);
    for (subject, path, object) in paths {
        graph_pattern = new_join(
            graph_pattern,
            GraphPattern::Path {
                subject,
                path,
                object,
            },
        )
    }
    graph_pattern
}

enum TripleOrPathPattern {
    Triple(TriplePattern),
    Path {
        subject: TermPattern,
        path: PropertyPathExpression,
        object: TermPattern,
    },
}

impl From<TriplePattern> for TripleOrPathPattern {
    fn from(tp: TriplePattern) -> Self {
        TripleOrPathPattern::Triple(tp)
    }
}

struct AnnotatedTermPath {
    term: TermPattern,
    annotations: Vec<(VariableOrPropertyPath, Vec<AnnotatedTermPath>)>,
}

impl From<AnnotatedTerm> for AnnotatedTermPath {
    fn from(term: AnnotatedTerm) -> Self {
        Self {
            term: term.term,
            annotations: term
                .annotations
                .into_iter()
                .map(|(p, o)| {
                    (
                        p.into(),
                        o.into_iter().map(AnnotatedTermPath::from).collect(),
                    )
                })
                .collect(),
        }
    }
}

#[derive(Default)]
struct FocusedTripleOrPathPattern<F> {
    focus: F,
    patterns: Vec<TripleOrPathPattern>,
}

impl<F> FocusedTripleOrPathPattern<F> {
    fn new(focus: F) -> Self {
        Self {
            focus,
            patterns: Vec::new(),
        }
    }
}

impl<F> From<FocusedTripleOrPathPattern<F>> for FocusedTripleOrPathPattern<Vec<F>> {
    fn from(input: FocusedTripleOrPathPattern<F>) -> Self {
        Self {
            focus: vec![input.focus],
            patterns: input.patterns,
        }
    }
}

impl<F, T: From<F>> From<FocusedTriplePattern<F>> for FocusedTripleOrPathPattern<T> {
    fn from(input: FocusedTriplePattern<F>) -> Self {
        Self {
            focus: input.focus.into(),
            patterns: input.patterns.into_iter().map(|p| p.into()).collect(),
        }
    }
}

#[derive(Eq, PartialEq, Debug, Clone, Hash)]
enum PartialGraphPattern {
    Optional(GraphPattern, Option<Expression>),
    Minus(GraphPattern),
    Bind(Expression, Variable),
    Filter(Expression),
    Other(GraphPattern),
}

fn new_join(l: GraphPattern, r: GraphPattern) -> GraphPattern {
    //Avoid to output empty BGPs
    if let GraphPattern::Bgp(pl) = &l {
        if pl.is_empty() {
            return r;
        }
    }
    if let GraphPattern::Bgp(pr) = &r {
        if pr.is_empty() {
            return l;
        }
    }

    //Merge BGPs
    match (l, r) {
        (GraphPattern::Bgp(mut pl), GraphPattern::Bgp(pr)) => {
            pl.extend(pr);
            GraphPattern::Bgp(pl)
        }
        (
            GraphPattern::Graph {
                graph_name: g1,
                inner: l,
            },
            GraphPattern::Graph {
                graph_name: g2,
                inner: r,
            },
        ) if g1 == g2 => {
            // We merge identical graphs
            GraphPattern::Graph {
                graph_name: g1,
                inner: Box::new(new_join(*l, *r)),
            }
        }
        (l, r) => GraphPattern::Join {
            left: Box::new(l),
            right: Box::new(r),
        },
    }
}

fn not_empty_fold<T>(
    iter: impl Iterator<Item = T>,
    combine: impl Fn(T, T) -> T,
) -> Result<T, &'static str> {
    iter.fold(None, |a, b| match a {
        Some(av) => Some(combine(av, b)),
        None => Some(b),
    })
    .ok_or("The iterator should not be empty")
}

enum SelectionOption {
    Distinct,
    Reduced,
    Default,
}

enum SelectionMember {
    Variable(Variable),
    Expression(Expression, Variable),
}

struct Selection {
    pub option: SelectionOption,
    pub variables: Option<Vec<SelectionMember>>,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            option: SelectionOption::Default,
            variables: None,
        }
    }
}

fn build_select(
    select: Selection,
    wher: GraphPattern,
    mut group: Option<(Vec<Variable>, Vec<(Expression, Variable)>)>,
    having: Option<Expression>,
    order_by: Option<Vec<OrderComparator>>,
    offset_limit: Option<(usize, Option<usize>)>,
    values: Option<GraphPattern>,
    state: &mut ParserState,
) -> GraphPattern {
    let mut p = wher;

    //GROUP BY
    let aggregates = state.aggregates.pop().unwrap_or_else(Vec::default);
    if group.is_none() && !aggregates.is_empty() {
        let const_variable = variable();
        group = Some((
            vec![const_variable.clone()],
            vec![(
                Literal::Typed {
                    value: "1".into(),
                    datatype: iri("http://www.w3.org/2001/XMLSchema#integer"),
                }
                .into(),
                const_variable,
            )],
        ));
    }

    if let Some((clauses, binds)) = group {
        for (expr, var) in binds {
            p = GraphPattern::Extend {
                inner: Box::new(p),
                var,
                expr,
            };
        }
        p = GraphPattern::Group {
            inner: Box::new(p),
            by: clauses,
            aggregates,
        };
    }

    //HAVING
    if let Some(expr) = having {
        p = GraphPattern::Filter {
            expr,
            inner: Box::new(p),
        };
    }

    //VALUES
    if let Some(data) = values {
        p = new_join(p, data);
    }

    //SELECT
    let mut pv: Vec<Variable> = Vec::new();
    match select.variables {
        Some(sel_items) => {
            for sel_item in sel_items {
                match sel_item {
                    SelectionMember::Variable(v) => pv.push(v),
                    SelectionMember::Expression(expr, v) => {
                        if pv.contains(&v) {
                            //TODO: fail
                        } else {
                            p = GraphPattern::Extend {
                                inner: Box::new(p),
                                var: v.clone(),
                                expr,
                            };
                            pv.push(v);
                        }
                    }
                }
            }
        }
        None => {
            pv.extend(p.visible_variables().into_iter().cloned()) //TODO: is it really useful to do a projection?
        }
    }
    let mut m = p;

    //ORDER BY
    if let Some(condition) = order_by {
        m = GraphPattern::OrderBy {
            inner: Box::new(m),
            condition,
        };
    }

    //PROJECT
    m = GraphPattern::Project {
        inner: Box::new(m),
        projection: pv,
    };
    match select.option {
        SelectionOption::Distinct => m = GraphPattern::Distinct { inner: Box::new(m) },
        SelectionOption::Reduced => m = GraphPattern::Reduced { inner: Box::new(m) },
        SelectionOption::Default => (),
    }

    //OFFSET LIMIT
    if let Some((start, length)) = offset_limit {
        m = GraphPattern::Slice {
            inner: Box::new(m),
            start,
            length,
        }
    }
    m
}

fn copy_graph(from: impl Into<GraphName>, to: impl Into<GraphNamePattern>) -> GraphUpdateOperation {
    let bgp = GraphPattern::Bgp(vec![TriplePattern::new(
        Variable { name: "s".into() },
        Variable { name: "p".into() },
        Variable { name: "o".into() },
    )]);
    GraphUpdateOperation::DeleteInsert {
        delete: Vec::new(),
        insert: vec![QuadPattern::new(
            Variable { name: "s".into() },
            Variable { name: "p".into() },
            Variable { name: "o".into() },
            to,
        )],
        using: None,
        pattern: Box::new(match from.into() {
            GraphName::NamedNode(from) => GraphPattern::Graph {
                graph_name: from.into(),
                inner: Box::new(bgp),
            },
            GraphName::DefaultGraph => bgp,
        }),
    }
}

enum Either<L, R> {
    Left(L),
    Right(R),
}

pub struct ParserState {
    base_iri: Option<Iri<String>>,
    namespaces: HashMap<String, String>,
    used_bnodes: HashSet<BlankNode>,
    currently_used_bnodes: HashSet<BlankNode>,
    aggregates: Vec<Vec<(Variable, AggregationFunction)>>,
}

impl ParserState {
    fn parse_iri(&self, iri: &str) -> Result<Iri<String>, IriParseError> {
        if let Some(base_iri) = &self.base_iri {
            base_iri.resolve(iri)
        } else {
            Iri::parse(iri.to_owned())
        }
    }

    fn new_aggregation(&mut self, agg: AggregationFunction) -> Result<Variable, &'static str> {
        let aggregates = self.aggregates.last_mut().ok_or("Unexpected aggregate")?;
        Ok(aggregates
            .iter()
            .find_map(|(v, a)| if a == &agg { Some(v) } else { None })
            .cloned()
            .unwrap_or_else(|| {
                let new_var = variable();
                aggregates.push((new_var.clone(), agg));
                new_var
            }))
    }
}

pub fn unescape_unicode_codepoints(input: &str) -> Cow<'_, str> {
    if needs_unescape_unicode_codepoints(input) {
        UnescapeUnicodeCharIterator::new(input).collect()
    } else {
        input.into()
    }
}

fn needs_unescape_unicode_codepoints(input: &str) -> bool {
    let bytes = input.as_bytes();
    for i in 1..bytes.len() {
        if (bytes[i] == b'u' || bytes[i] == b'U') && bytes[i - 1] == b'\\' {
            return true;
        }
    }
    false
}

struct UnescapeUnicodeCharIterator<'a> {
    iter: Chars<'a>,
    buffer: String,
}

impl<'a> UnescapeUnicodeCharIterator<'a> {
    fn new(string: &'a str) -> Self {
        Self {
            iter: string.chars(),
            buffer: String::with_capacity(9),
        }
    }
}

impl<'a> Iterator for UnescapeUnicodeCharIterator<'a> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        if !self.buffer.is_empty() {
            return Some(self.buffer.remove(0));
        }
        match self.iter.next()? {
            '\\' => match self.iter.next() {
                Some('u') => {
                    self.buffer.push('u');
                    for _ in 0..4 {
                        if let Some(c) = self.iter.next() {
                            self.buffer.push(c);
                        } else {
                            return Some('\\');
                        }
                    }
                    if let Some(c) = u32::from_str_radix(&self.buffer[1..5], 16)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        self.buffer.clear();
                        Some(c)
                    } else {
                        Some('\\')
                    }
                }
                Some('U') => {
                    self.buffer.push('U');
                    for _ in 0..8 {
                        if let Some(c) = self.iter.next() {
                            self.buffer.push(c);
                        } else {
                            return Some('\\');
                        }
                    }
                    if let Some(c) = u32::from_str_radix(&self.buffer[1..9], 16)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        self.buffer.clear();
                        Some(c)
                    } else {
                        Some('\\')
                    }
                }
                Some(c) => {
                    self.buffer.push(c);
                    Some('\\')
                }
                None => Some('\\'),
            },
            c => Some(c),
        }
    }
}

pub fn unescape_characters<'a>(
    input: &'a str,
    characters: &'static [u8],
    replacement: &'static StaticCharSliceMap,
) -> Cow<'a, str> {
    if needs_unescape_characters(input, characters) {
        UnescapeCharsIterator::new(input, replacement).collect()
    } else {
        input.into()
    }
}

fn needs_unescape_characters(input: &str, characters: &[u8]) -> bool {
    let bytes = input.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i - 1] == b'\\' && characters.contains(&bytes[i]) {
            return true;
        }
    }
    false
}

struct UnescapeCharsIterator<'a> {
    iter: Chars<'a>,
    buffer: Option<char>,
    replacement: &'static StaticCharSliceMap,
}

impl<'a> UnescapeCharsIterator<'a> {
    fn new(string: &'a str, replacement: &'static StaticCharSliceMap) -> Self {
        Self {
            iter: string.chars(),
            buffer: None,
            replacement,
        }
    }
}

impl<'a> Iterator for UnescapeCharsIterator<'a> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        if let Some(ch) = self.buffer {
            self.buffer = None;
            return Some(ch);
        }
        match self.iter.next()? {
            '\\' => match self.iter.next() {
                Some(ch) => match self.replacement.get(ch) {
                    Some(replace) => Some(replace),
                    None => {
                        self.buffer = Some(ch);
                        Some('\\')
                    }
                },
                None => Some('\\'),
            },
            c => Some(c),
        }
    }
}

pub struct StaticCharSliceMap {
    keys: &'static [char],
    values: &'static [char],
}

impl StaticCharSliceMap {
    pub const fn new(keys: &'static [char], values: &'static [char]) -> Self {
        Self { keys, values }
    }

    pub fn get(&self, key: char) -> Option<char> {
        for i in 0..self.keys.len() {
            if self.keys[i] == key {
                return Some(self.values[i]);
            }
        }
        None
    }
}

const UNESCAPE_CHARACTERS: [u8; 8] = [b't', b'b', b'n', b'r', b'f', b'"', b'\'', b'\\'];
const UNESCAPE_REPLACEMENT: StaticCharSliceMap = StaticCharSliceMap::new(
    &['t', 'b', 'n', 'r', 'f', '"', '\'', '\\'],
    &[
        '\u{0009}', '\u{0008}', '\u{000A}', '\u{000D}', '\u{000C}', '\u{0022}', '\u{0027}',
        '\u{005C}',
    ],
);

fn unescape_echars(input: &str) -> Cow<'_, str> {
    unescape_characters(input, &UNESCAPE_CHARACTERS, &UNESCAPE_REPLACEMENT)
}

const UNESCAPE_PN_CHARACTERS: [u8; 20] = [
    b'_', b'~', b'.', b'-', b'!', b'$', b'&', b'\'', b'(', b')', b'*', b'+', b',', b';', b'=',
    b'/', b'?', b'#', b'@', b'%',
];
const UNESCAPE_PN_REPLACEMENT: StaticCharSliceMap = StaticCharSliceMap::new(
    &[
        '_', '~', '.', '-', '!', '$', '&', '\'', '(', ')', '*', '+', ',', ';', '=', '/', '?', '#',
        '@', '%',
    ],
    &[
        '_', '~', '.', '-', '!', '$', '&', '\'', '(', ')', '*', '+', ',', ';', '=', '/', '?', '#',
        '@', '%',
    ],
);

pub fn unescape_pn_local(input: &str) -> Cow<'_, str> {
    unescape_characters(input, &UNESCAPE_PN_CHARACTERS, &UNESCAPE_PN_REPLACEMENT)
}

fn iri(value: impl Into<String>) -> NamedNode {
    NamedNode { iri: value.into() }
}

fn bnode() -> BlankNode {
    BlankNode {
        id: format!("{:x}", random::<u128>()),
    }
}

fn variable() -> Variable {
    Variable {
        name: format!("{:x}", random::<u128>()),
    }
}

parser! {
    //See https://www.w3.org/TR/turtle/#sec-grammar
    grammar parser(state: &mut ParserState) for str {
        //[1]
        pub rule QueryUnit() -> Query = Query()

        //[2]
        rule Query() -> Query = _ Prologue() _ q:(SelectQuery() / ConstructQuery() / DescribeQuery() / AskQuery()) _ {
            q
        }

        //[3]
        pub rule UpdateInit() -> Vec<GraphUpdateOperation> = Update()

        //[4]
        rule Prologue() = (BaseDecl() _ / PrefixDecl() _)* {}

        //[5]
        rule BaseDecl() = i("BASE") _ i:IRIREF() {
            state.base_iri = Some(i)
        }

        //[6]
        rule PrefixDecl() = i("PREFIX") _ ns:PNAME_NS() _ i:IRIREF() {
            state.namespaces.insert(ns.into(), i.into_inner());
        }

        //[7]
        rule SelectQuery() -> Query = s:SelectClause() _ d:DatasetClauses() _ w:WhereClause() _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {
            Query::Select {
                dataset: d,
                pattern: build_select(s, w, g, h, o, l, v, state),
                base_iri: state.base_iri.clone()
            }
        }

        //[8]
        rule SubSelect() -> GraphPattern = s:SelectClause() _ w:WhereClause() _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {
            build_select(s, w, g, h, o, l, v, state)
        }

        //[9]
        rule SelectClause() -> Selection = i("SELECT") _ Selection_init() o:SelectClause_option() _ v:SelectClause_variables() {
            Selection {
                option: o,
                variables: v
            }
        }
        rule Selection_init() = {
            state.aggregates.push(Vec::new())
        }
        rule SelectClause_option() -> SelectionOption =
            i("DISTINCT") { SelectionOption::Distinct } /
            i("REDUCED") { SelectionOption::Reduced } /
            { SelectionOption::Default }
        rule SelectClause_variables() -> Option<Vec<SelectionMember>> =
            "*" { None } /
            p:SelectClause_member()+ { Some(p) }
        rule SelectClause_member() -> SelectionMember =
            v:Var() _ { SelectionMember::Variable(v) } /
            "(" _ e:Expression() _ i("AS") _ v:Var() _ ")" _ { SelectionMember::Expression(e, v) }

        //[10]
        rule ConstructQuery() -> Query =
            i("CONSTRUCT") _ c:ConstructTemplate() _ d:DatasetClauses() _ w:WhereClause() _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {
                Query::Construct {
                    template: c,
                    dataset: d,
                    pattern: build_select(Selection::default(), w, g, h, o, l, v, state),
                    base_iri: state.base_iri.clone()
                }
            } /
            i("CONSTRUCT") _ d:DatasetClauses() _ i("WHERE") _ "{" _ c:ConstructQuery_optional_triple_template() _ "}" _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {
                Query::Construct {
                    template: c.clone(),
                    dataset: d,
                    pattern: build_select(
                        Selection::default(),
                        GraphPattern::Bgp(c),
                        g, h, o, l, v, state
                    ),
                    base_iri: state.base_iri.clone()
                }
            }

        rule ConstructQuery_optional_triple_template() -> Vec<TriplePattern> = TriplesTemplate() / { Vec::new() }

        //[11]
        rule DescribeQuery() -> Query =
            i("DESCRIBE") _ "*" _ d:DatasetClauses() w:WhereClause()? _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {
                Query::Describe {
                    dataset: d,
                    pattern: build_select(Selection::default(), w.unwrap_or_else(GraphPattern::default), g, h, o, l, v, state),
                    base_iri: state.base_iri.clone()
                }
            } /
            i("DESCRIBE") _ p:DescribeQuery_item()+ _ d:DatasetClauses() w:WhereClause()? _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {
                Query::Describe {
                    dataset: d,
                    pattern: build_select(Selection {
                        option: SelectionOption::Default,
                        variables: Some(p.into_iter().map(|var_or_iri| match var_or_iri {
                            NamedNodePattern::NamedNode(n) => SelectionMember::Expression(n.into(), variable()),
                            NamedNodePattern::Variable(v) => SelectionMember::Variable(v)
                        }).collect())
                    }, w.unwrap_or_else(GraphPattern::default), g, h, o, l, v, state),
                    base_iri: state.base_iri.clone()
                }
            }
        rule DescribeQuery_item() -> NamedNodePattern = i:VarOrIri() _ { i }

        //[12]
        rule AskQuery() -> Query = i("ASK") _ d:DatasetClauses() w:WhereClause() _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {
            Query::Ask {
                dataset: d,
                pattern: build_select(Selection::default(), w, g, h, o, l, v, state),
                base_iri: state.base_iri.clone()
            }
        }

        //[13]
        rule DatasetClause() -> (Option<NamedNode>, Option<NamedNode>) = i("FROM") _ d:(DefaultGraphClause() / NamedGraphClause()) { d }
        rule DatasetClauses() -> Option<QueryDataset> = d:DatasetClause() ** (_) {
            if d.is_empty() {
                return None;
            }
            let mut default = Vec::new();
            let mut named = Vec::new();
            for (d, n) in d {
                if let Some(d) = d {
                    default.push(d);
                }
                if let Some(n) = n {
                    named.push(n);
                }
            }
            Some(QueryDataset {
                default, named: Some(named)
            })
        }

        //[14]
        rule DefaultGraphClause() -> (Option<NamedNode>, Option<NamedNode>) = s:SourceSelector() {
            (Some(s), None)
        }

        //[15]
        rule NamedGraphClause() -> (Option<NamedNode>, Option<NamedNode>) = i("NAMED") _ s:SourceSelector() {
            (None, Some(s))
        }

        //[16]
        rule SourceSelector() -> NamedNode = iri()

        //[17]
        rule WhereClause() -> GraphPattern = i("WHERE")? _ p:GroupGraphPattern() {
            p
        }

        //[19]
        rule GroupClause() -> (Vec<Variable>, Vec<(Expression,Variable)>) = i("GROUP") _ i("BY") _ c:GroupCondition_item()+ {
            let mut projections: Vec<(Expression,Variable)> = Vec::new();
            let clauses = c.into_iter().map(|(e, vo)| {
                if let Expression::Variable(v) = e {
                    v
                } else {
                    let v = vo.unwrap_or_else(variable);
                    projections.push((e, v.clone()));
                    v
                }
            }).collect();
            (clauses, projections)
        }
        rule GroupCondition_item() -> (Expression, Option<Variable>) = c:GroupCondition() _ { c }

        //[20]
        rule GroupCondition() -> (Expression, Option<Variable>) =
            e:BuiltInCall() { (e, None) } /
            e:FunctionCall() { (e, None) } /
            "(" _ e:Expression() _ v:GroupCondition_as()? ")" { (e, v) } /
            e:Var() { (e.into(), None) }
        rule GroupCondition_as() -> Variable = i("AS") _ v:Var() _ { v }

        //[21]
        rule HavingClause() -> Expression = i("HAVING") _ e:HavingCondition()+ {?
            not_empty_fold(e.into_iter(), |a, b| Expression::And(Box::new(a), Box::new(b)))
        }

        //[22]
        rule HavingCondition() -> Expression = Constraint()

        //[23]
        rule OrderClause() -> Vec<OrderComparator> = i("ORDER") _ i("BY") _ c:OrderClause_item()+ { c }
        rule OrderClause_item() -> OrderComparator = c:OrderCondition() _ { c }

        //[24]
        rule OrderCondition() -> OrderComparator =
            i("ASC") _ e: BrackettedExpression() { OrderComparator::Asc(e) } /
            i("DESC") _ e: BrackettedExpression() { OrderComparator::Desc(e) } /
            e: Constraint() { OrderComparator::Asc(e) } /
            v: Var() { OrderComparator::Asc(Expression::from(v)) }

        //[25]
        rule LimitOffsetClauses() -> (usize, Option<usize>) =
            l:LimitClause() _ o:OffsetClause()? { (o.unwrap_or(0), Some(l)) } /
            o:OffsetClause() _ l:LimitClause()? { (o, l) }

        //[26]
        rule LimitClause() -> usize = i("LIMIT") _ l:$(INTEGER()) {?
            usize::from_str(l).map_err(|_| "The query limit should be a non negative integer")
        }

        //[27]
        rule OffsetClause() -> usize = i("OFFSET") _ o:$(INTEGER()) {?
            usize::from_str(o).map_err(|_| "The query offset should be a non negative integer")
        }

        //[28]
        rule ValuesClause() -> Option<GraphPattern> =
            i("VALUES") _ p:DataBlock() { Some(p) } /
            { None }


        //[29]
        rule Update() -> Vec<GraphUpdateOperation> = _ Prologue() _ u:(Update1() ** (_ ";" _))  _ ( ";" _)? { u.into_iter().flatten().collect() }

        //[30]
        rule Update1() -> Vec<GraphUpdateOperation> = Load() / Clear() / Drop() / Add() / Move() / Copy() / Create() / InsertData() / DeleteData() / DeleteWhere() / Modify()
        rule Update1_silent() -> bool = i("SILENT") { true } / { false }

        //[31]
        rule Load() -> Vec<GraphUpdateOperation> = i("LOAD") _ silent:Update1_silent() _ from:iri() _ to:Load_to()? {
            vec![GraphUpdateOperation::Load { silent, from, to: to.map_or(GraphName::DefaultGraph, GraphName::NamedNode) }]
        }
        rule Load_to() -> NamedNode = i("INTO") _ g: GraphRef() { g }

        //[32]
        rule Clear() -> Vec<GraphUpdateOperation> = i("CLEAR") _ silent:Update1_silent() _ graph:GraphRefAll() {
            vec![GraphUpdateOperation::Clear { silent, graph }]
        }

        //[33]
        rule Drop() -> Vec<GraphUpdateOperation> = i("DROP") _ silent:Update1_silent() _ graph:GraphRefAll() {
            vec![GraphUpdateOperation::Drop { silent, graph }]
        }

        //[34]
        rule Create() -> Vec<GraphUpdateOperation> = i("CREATE") _ silent:Update1_silent() _ graph:GraphRef() {
            vec![GraphUpdateOperation::Create { silent, graph }]
        }

        //[35]
        rule Add() -> Vec<GraphUpdateOperation> = i("ADD") _ silent:Update1_silent() _ from:GraphOrDefault() _ i("TO") _ to:GraphOrDefault() {
            // Rewriting defined by https://www.w3.org/TR/sparql11-update/#add
            if from == to {
                Vec::new() // identity case
            } else {
                let bgp = GraphPattern::Bgp(vec![TriplePattern::new(Variable { name: "s".into() }, Variable { name: "p".into() }, Variable { name: "o".into() })]);
                vec![copy_graph(from, to)]
            }
        }

        //[36]
        rule Move() -> Vec<GraphUpdateOperation> = i("MOVE") _ silent:Update1_silent() _ from:GraphOrDefault() _ i("TO") _ to:GraphOrDefault() {
            // Rewriting defined by https://www.w3.org/TR/sparql11-update/#move
            if from == to {
                Vec::new() // identity case
            } else {
                let bgp = GraphPattern::Bgp(vec![TriplePattern::new(Variable { name: "s".into() }, Variable { name: "p".into() }, Variable { name: "o".into() })]);
                vec![GraphUpdateOperation::Drop { silent: true, graph: to.clone().into() }, copy_graph(from.clone(), to), GraphUpdateOperation::Drop { silent, graph: from.into() }]
            }
        }

        //[37]
        rule Copy() -> Vec<GraphUpdateOperation> = i("COPY") _ silent:Update1_silent() _ from:GraphOrDefault() _ i("TO") _ to:GraphOrDefault() {
            // Rewriting defined by https://www.w3.org/TR/sparql11-update/#copy
            if from == to {
                Vec::new() // identity case
            } else {
                let bgp = GraphPattern::Bgp(vec![TriplePattern::new(Variable { name: "s".into() }, Variable { name: "p".into() }, Variable{ name: "o".into() })]);
                vec![GraphUpdateOperation::Drop { silent: true, graph: to.clone().into() }, copy_graph(from, to)]
            }
        }

        //[38]
        rule InsertData() -> Vec<GraphUpdateOperation> = i("INSERT") _ i("DATA") _ data:QuadData() {
            vec![GraphUpdateOperation::InsertData { data }]
        }

        //[39]
        rule DeleteData() -> Vec<GraphUpdateOperation> = i("DELETE") _ i("DATA") _ data:GroundQuadData() {
            vec![GraphUpdateOperation::DeleteData { data }]
        }

        //[40]
        rule DeleteWhere() -> Vec<GraphUpdateOperation> = i("DELETE") _ i("WHERE") _ d:QuadPattern() {?
                        let pattern = d.iter().map(|q| {
                let bgp = GraphPattern::Bgp(vec![TriplePattern::new(q.subject.clone(), q.predicate.clone(), q.object.clone())]);
                match &q.graph_name {
                    GraphNamePattern::NamedNode(graph_name) => GraphPattern::Graph { graph_name: graph_name.clone().into(), inner: Box::new(bgp) },
                    GraphNamePattern::DefaultGraph => bgp,
                    GraphNamePattern::Variable(graph_name) => GraphPattern::Graph { graph_name: graph_name.clone().into(), inner: Box::new(bgp) },
                }
            }).fold(GraphPattern::Bgp(Vec::new()), new_join);
            let delete = d.into_iter().map(GroundQuadPattern::try_from).collect::<Result<Vec<_>,_>>().map_err(|_| "Blank nodes are not allowed in DELETE WHERE")?;
            Ok(vec![GraphUpdateOperation::DeleteInsert {
                delete,
                insert: Vec::new(),
                using: None,
                pattern: Box::new(pattern)
            }])
        }

        //[41]
        rule Modify() -> Vec<GraphUpdateOperation> = with:Modify_with()? _ Modify_clear() c:Modify_clauses() _ u:(UsingClause() ** (_)) _ i("WHERE") _ pattern:GroupGraphPattern() {
            let (delete, insert) = c;
            let mut delete = delete.unwrap_or_else(Vec::new);
            let mut insert = insert.unwrap_or_else(Vec::new);
            let mut pattern = pattern;

            let mut using = if u.is_empty() {
                None
            } else {
                let mut default = Vec::new();
                let mut named = Vec::new();
                for (d, n) in u {
                    if let Some(d) = d {
                        default.push(d)
                    }
                    if let Some(n) = n {
                        named.push(n)
                    }
                }
                Some(QueryDataset { default, named: Some(named) })
            };

            if let Some(with) = with {
                // We inject WITH everywhere
                delete = delete.into_iter().map(|q| if q.graph_name == GraphNamePattern::DefaultGraph {
                    GroundQuadPattern {
                        subject: q.subject,
                        predicate: q.predicate,
                        object: q.object,
                        graph_name: with.clone().into()
                    }
                } else {
                    q
                }).collect();
                insert = insert.into_iter().map(|q| if q.graph_name == GraphNamePattern::DefaultGraph {
                    QuadPattern {
                        subject: q.subject,
                        predicate: q.predicate,
                        object: q.object,
                        graph_name: with.clone().into()
                    }
                } else {
                    q
                }).collect();
                if using.is_none() {
                    using = Some(QueryDataset { default: vec![with], named: None });
                }
            }

            vec![GraphUpdateOperation::DeleteInsert {
                delete,
                insert,
                using,
                pattern: Box::new(pattern)
            }]
        }
        rule Modify_with() -> NamedNode = i("WITH") _ i:iri() _ { i }
        rule Modify_clauses() -> (Option<Vec<GroundQuadPattern>>, Option<Vec<QuadPattern>>) = d:DeleteClause() _ i:InsertClause()? {
            (Some(d), i)
        } / i:InsertClause() {
            (None, Some(i))
        }
        rule Modify_clear() -> () = {
            state.used_bnodes.clear();
            state.currently_used_bnodes.clear();
        }

        //[42]
        rule DeleteClause() -> Vec<GroundQuadPattern> = i("DELETE") _ q:QuadPattern() {?
            q.into_iter().map(GroundQuadPattern::try_from).collect::<Result<Vec<_>,_>>().map_err(|_| "Blank nodes are not allowed in DELETE WHERE")
        }

        //[43]
        rule InsertClause() -> Vec<QuadPattern> = i("INSERT") _ q:QuadPattern() { q }

        //[44]
        rule UsingClause() -> (Option<NamedNode>, Option<NamedNode>) = i("USING") _ d:(UsingClause_default() / UsingClause_named()) { d }
        rule UsingClause_default() -> (Option<NamedNode>, Option<NamedNode>) = i:iri() {
            (Some(i), None)
        }
        rule UsingClause_named() -> (Option<NamedNode>, Option<NamedNode>) = i("NAMED") _ i:iri() {
            (None, Some(i))
        }

        //[45]
        rule GraphOrDefault() -> GraphName = i("DEFAULT") {
            GraphName::DefaultGraph
        } / (i("GRAPH") _)? g:iri() {
            GraphName::NamedNode(g)
        }

        //[46]
        rule GraphRef() -> NamedNode = i("GRAPH") _ g:iri() { g }

        //[47]
        rule GraphRefAll() -> GraphTarget  = i: GraphRef() { i.into() }
            / i("DEFAULT") { GraphTarget::DefaultGraph }
            / i("NAMED") { GraphTarget::NamedGraphs }
            / i("ALL") { GraphTarget::AllGraphs }

        //[48]
        rule QuadPattern() -> Vec<QuadPattern> = "{" _ q:Quads() _ "}" { q }

        //[49]
        rule QuadData() -> Vec<Quad> = "{" _ q:Quads() _ "}" {?
            q.into_iter().map(Quad::try_from).collect::<Result<Vec<_>, ()>>().map_err(|_| "Variables are not allowed in INSERT DATA")
        }
        rule GroundQuadData() -> Vec<GroundQuad> = "{" _ q:Quads() _ "}" {?
            q.into_iter().map(|q| GroundQuad::try_from(Quad::try_from(q)?)).collect::<Result<Vec<_>, ()>>().map_err(|_| "Variables and blank nodes are not allowed in DELETE DATA")
        }

        //[50]
        rule Quads() -> Vec<QuadPattern> = q:(Quads_TriplesTemplate() / Quads_QuadsNotTriples()) ** (_) {
            q.into_iter().flatten().collect()
        }
        rule Quads_TriplesTemplate() -> Vec<QuadPattern> = t:TriplesTemplate() {
            t.into_iter().map(|t| QuadPattern::new(t.subject, t.predicate, t.object, GraphNamePattern::DefaultGraph)).collect()
        } //TODO: return iter?
        rule Quads_QuadsNotTriples() -> Vec<QuadPattern> = q:QuadsNotTriples() _ "."? { q }

        //[51]
        rule QuadsNotTriples() -> Vec<QuadPattern> = i("GRAPH") _ g:VarOrIri() _ "{" _ t:TriplesTemplate()? _ "}" {
            t.unwrap_or_else(Vec::new).into_iter().map(|t| QuadPattern::new(t.subject, t.predicate, t.object, g.clone())).collect()
        }

        //[52]
        rule TriplesTemplate() -> Vec<TriplePattern> =  h:TriplesSameSubject() _ t:TriplesTemplate_tail()? {
            let mut triples = h;
            if let Some(l) = t {
                triples.extend(l)
            }
            triples
        }
        rule TriplesTemplate_tail() -> Vec<TriplePattern> = "." _ t:TriplesTemplate()? _ {
            t.unwrap_or_else(Vec::default)
        }

        //[53]
        rule GroupGraphPattern() -> GraphPattern =
            "{" _ p:GroupGraphPatternSub() _ "}" { p } /
            "{" _ p:SubSelect() _ "}" { p }

        //[54]
        rule GroupGraphPatternSub() -> GraphPattern = a:TriplesBlock()? _ b:GroupGraphPatternSub_item()* {
            let mut p = a.map_or_else(Vec::default, |v| vec![PartialGraphPattern::Other(build_bgp(v))]);
            for v in b {
                p.extend(v)
            }
            let mut filter: Option<Expression> = None;
            let mut g = GraphPattern::default();
            for e in p {
                match e {
                    PartialGraphPattern::Optional(p, f) => {
                        g = GraphPattern::LeftJoin { left: Box::new(g), right: Box::new(p), expr: f }
                    }
                    PartialGraphPattern::Minus(p) => {
                        g = GraphPattern::Minus { left: Box::new(g), right: Box::new(p) }
                    }
                    PartialGraphPattern::Bind(expr, var) => {
                        g = GraphPattern::Extend { inner: Box::new(g), var, expr }
                    }
                    PartialGraphPattern::Filter(expr) => filter = Some(if let Some(f) = filter {
                        Expression::And(Box::new(f), Box::new(expr))
                    } else {
                        expr
                    }),
                    PartialGraphPattern::Other(e) => g = new_join(g, e),
                }
            }

            // We deal with blank nodes aliases rule (TODO: partial for now)
            state.used_bnodes.extend(state.currently_used_bnodes.iter().cloned());
            state.currently_used_bnodes.clear();

            if let Some(expr) = filter {
                GraphPattern::Filter { expr, inner: Box::new(g) }
            } else {
                g
            }
        }
        rule GroupGraphPatternSub_item() -> Vec<PartialGraphPattern> = a:GraphPatternNotTriples() _ ("." _)? b:TriplesBlock()? _ {
            let mut result = vec![a];
            if let Some(v) = b {
                result.push(PartialGraphPattern::Other(build_bgp(v)));
            }
            result
        }

        //[55]
        rule TriplesBlock() -> Vec<TripleOrPathPattern> = h:TriplesSameSubjectPath() _ t:TriplesBlock_tail()? {
            let mut triples = h;
            if let Some(l) = t {
                triples.extend(l)
            }
            triples
        }
        rule TriplesBlock_tail() -> Vec<TripleOrPathPattern> = "." _ t:TriplesBlock()? _ {
            t.unwrap_or_else(Vec::default)
        }

        //[56]
        rule GraphPatternNotTriples() -> PartialGraphPattern = GroupOrUnionGraphPattern() / OptionalGraphPattern() / MinusGraphPattern() / GraphGraphPattern() / ServiceGraphPattern() / Filter() / Bind() / InlineData()

        //[57]
        rule OptionalGraphPattern() -> PartialGraphPattern = i("OPTIONAL") _ p:GroupGraphPattern() {
            if let GraphPattern::Filter { expr, inner } =  p {
               PartialGraphPattern::Optional(*inner, Some(expr))
            } else {
               PartialGraphPattern::Optional(p, None)
            }
        }

        //[58]
        rule GraphGraphPattern() -> PartialGraphPattern = i("GRAPH") _ graph_name:VarOrIri() _ p:GroupGraphPattern() {
            PartialGraphPattern::Other(GraphPattern::Graph { graph_name, inner: Box::new(p) })
        }

        //[59]
        rule ServiceGraphPattern() -> PartialGraphPattern =
            i("SERVICE") _ i("SILENT") _ name:VarOrIri() _ p:GroupGraphPattern() { PartialGraphPattern::Other(GraphPattern::Service { name, pattern: Box::new(p), silent: true }) } /
            i("SERVICE") _ name:VarOrIri() _ p:GroupGraphPattern() { PartialGraphPattern::Other(GraphPattern::Service{ name, pattern: Box::new(p), silent: true }) }

        //[60]
        rule Bind() -> PartialGraphPattern = i("BIND") _ "(" _ e:Expression() _ i("AS") _ v:Var() _ ")" {
            PartialGraphPattern::Bind(e, v)
        }

        //[61]
        rule InlineData() -> PartialGraphPattern = i("VALUES") _ p:DataBlock() { PartialGraphPattern::Other(p) }

        //[62]
        rule DataBlock() -> GraphPattern = l:(InlineDataOneVar() / InlineDataFull()) {
            GraphPattern::Table { variables: l.0, rows: l.1 }
        }

        //[63]
        rule InlineDataOneVar() -> (Vec<Variable>, Vec<Vec<Option<GroundTerm>>>) = var:Var() _ "{" _ d:InlineDataOneVar_value()* "}" {
            (vec![var], d)
        }
        rule InlineDataOneVar_value() -> Vec<Option<GroundTerm>> = t:DataBlockValue() _ { vec![t] }

        //[64]
        rule InlineDataFull() -> (Vec<Variable>, Vec<Vec<Option<GroundTerm>>>) = "(" _ vars:InlineDataFull_var()* _ ")" _ "{" _ val:InlineDataFull_values()* "}" {
            (vars, val)
        }
        rule InlineDataFull_var() -> Variable = v:Var() _ { v }
        rule InlineDataFull_values() -> Vec<Option<GroundTerm>> = "(" _ v:InlineDataFull_value()* _ ")" _ { v }
        rule InlineDataFull_value() -> Option<GroundTerm> = v:DataBlockValue() _ { v }

        //[65]
        rule DataBlockValue() -> Option<GroundTerm> =
            t:EmbTriple() { Some(t.into()) } /
            i:iri() { Some(i.into()) } /
            l:RDFLiteral() { Some(l.into()) } /
            l:NumericLiteral() { Some(l.into()) } /
            l:BooleanLiteral() { Some(l.into()) } /
            i("UNDEF") { None }

        //[66]
        rule MinusGraphPattern() -> PartialGraphPattern = i("MINUS") _ p: GroupGraphPattern() {
            PartialGraphPattern::Minus(p)
        }

        //[67]
        rule GroupOrUnionGraphPattern() -> PartialGraphPattern = p:GroupOrUnionGraphPattern_item() **<1,> (i("UNION") _) {?
            not_empty_fold(p.into_iter(), |a, b| {
                GraphPattern::Union { left: Box::new(a), right: Box::new(b) }
            }).map(PartialGraphPattern::Other)
        }
        rule GroupOrUnionGraphPattern_item() -> GraphPattern = p:GroupGraphPattern() _ { p }

        //[68]
        rule Filter() -> PartialGraphPattern = i("FILTER") _ c:Constraint() {
            PartialGraphPattern::Filter(c)
        }

        //[69]
        rule Constraint() -> Expression = BrackettedExpression() / FunctionCall() / BuiltInCall()

        //[70]
        rule FunctionCall() -> Expression = f: iri() _ a: ArgList() {
            Expression::FunctionCall(Function::Custom(f), a)
        }

        //[71]
        rule ArgList() -> Vec<Expression> =
            "(" _ e:ArgList_item() **<1,> ("," _) _ ")" { e } /
            NIL() { Vec::new() }
        rule ArgList_item() -> Expression = e:Expression() _ { e }

        //[72]
        rule ExpressionList() -> Vec<Expression> =
            "(" _ e:ExpressionList_item() **<1,> ("," _) ")" { e } /
            NIL() { Vec::new() }
        rule ExpressionList_item() -> Expression = e:Expression() _ { e }

        //[73]
        rule ConstructTemplate() -> Vec<TriplePattern> = "{" _ t:ConstructTriples() _ "}" { t }

        //[74]
        rule ConstructTriples() -> Vec<TriplePattern> = p:ConstructTriples_item() ** ("." _) "."? {
            p.into_iter().flat_map(|c| c.into_iter()).collect()
        }
        rule ConstructTriples_item() -> Vec<TriplePattern> = t:TriplesSameSubject() _ { t }

        //[75]
        rule TriplesSameSubject() -> Vec<TriplePattern> =
            s:VarOrTermOrEmbTP() _ po:PropertyListNotEmpty() {
                let mut patterns = po.patterns;
                for (p, os) in po.focus {
                    for o in os {
                        add_to_triple_patterns(s.clone(), p.clone(), o, &mut patterns)
                    }
                }
                patterns
            } /
            s:TriplesNode() _ po:PropertyList() {
                let mut patterns = s.patterns;
                patterns.extend(po.patterns);
                for (p, os) in po.focus {
                    for o in os {
                        add_to_triple_patterns(s.focus.clone(), p.clone(), o, &mut patterns)
                    }
                }
                patterns
            }

        //[76]
        rule PropertyList() -> FocusedTriplePattern<Vec<(NamedNodePattern,Vec<AnnotatedTerm>)>> =
            PropertyListNotEmpty() /
            { FocusedTriplePattern::default() }

        //[77]
        rule PropertyListNotEmpty() -> FocusedTriplePattern<Vec<(NamedNodePattern,Vec<AnnotatedTerm>)>> = l:PropertyListNotEmpty_item() **<1,> (";" _) {
            l.into_iter().fold(FocusedTriplePattern::<Vec<(NamedNodePattern,Vec<AnnotatedTerm>)>>::default(), |mut a, b| {
                a.focus.push(b.focus);
                a.patterns.extend(b.patterns);
                a
            })
        }
        rule PropertyListNotEmpty_item() -> FocusedTriplePattern<(NamedNodePattern,Vec<AnnotatedTerm>)> = p:Verb() _ o:ObjectList() _ {
            FocusedTriplePattern {
                focus: (p, o.focus),
                patterns: o.patterns
            }
        }

        //[78]
        rule Verb() -> NamedNodePattern = VarOrIri() / "a" { iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type").into() }

        //[79]
        rule ObjectList() -> FocusedTriplePattern<Vec<AnnotatedTerm>> = o:ObjectList_item() **<1,> ("," _) {
            o.into_iter().fold(FocusedTriplePattern::<Vec<AnnotatedTerm>>::default(), |mut a, b| {
                a.focus.push(b.focus);
                a.patterns.extend_from_slice(&b.patterns);
                a
            })
        }
        rule ObjectList_item() -> FocusedTriplePattern<AnnotatedTerm> = o:Object() _ { o }

        //[80]
        rule Object() -> FocusedTriplePattern<AnnotatedTerm> = g:GraphNode() _ a:AnnotationPattern()? {
            if let Some(a) = a {
                let mut patterns = g.patterns;
                patterns.extend(a.patterns);
                FocusedTriplePattern {
                    focus: AnnotatedTerm {
                        term: g.focus,
                        annotations: a.focus
                    },
                    patterns
                }
            } else {
                FocusedTriplePattern {
                    focus: AnnotatedTerm {
                        term: g.focus,
                        annotations: Vec::new()
                    },
                    patterns: g.patterns
                }
            }
        }

        //[81]
        rule TriplesSameSubjectPath() -> Vec<TripleOrPathPattern> =
            s:VarOrTermOrEmbTP() _ po:PropertyListPathNotEmpty() {?
                let mut patterns = po.patterns;
                for (p, os) in po.focus {
                    for o in os {
                        add_to_triple_or_path_patterns(s.clone(), p.clone(), o, &mut patterns)?;
                    }
                }
                Ok(patterns)
            } /
            s:TriplesNodePath() _ po:PropertyListPath() {?
                let mut patterns = s.patterns;
                    patterns.extend(po.patterns);
                for (p, os) in po.focus {
                    for o in os {
                        add_to_triple_or_path_patterns(s.focus.clone(), p.clone(), o, &mut patterns)?;
                    }
                }
                Ok(patterns)
            }

        //[82]
        rule PropertyListPath() -> FocusedTripleOrPathPattern<Vec<(VariableOrPropertyPath,Vec<AnnotatedTermPath>)>> =
            PropertyListPathNotEmpty() /
            { FocusedTripleOrPathPattern::default() }

        //[83]
        rule PropertyListPathNotEmpty() -> FocusedTripleOrPathPattern<Vec<(VariableOrPropertyPath,Vec<AnnotatedTermPath>)>> = hp:(VerbPath() / VerbSimple()) _ ho:ObjectListPath() _ t:PropertyListPathNotEmpty_item()* {
                t.into_iter().flat_map(|e| e.into_iter()).fold(FocusedTripleOrPathPattern {
                    focus: vec![(hp, ho.focus)],
                    patterns: ho.patterns
                }, |mut a, b| {
                    a.focus.push(b.focus);
                    a.patterns.extend(b.patterns.into_iter().map(|v| v.into()));
                    a
                })
        }
        rule PropertyListPathNotEmpty_item() -> Option<FocusedTriplePattern<(VariableOrPropertyPath,Vec<AnnotatedTermPath>)>> = ";" _ c:PropertyListPathNotEmpty_item_content()? {
            c
        }
        rule PropertyListPathNotEmpty_item_content() -> FocusedTriplePattern<(VariableOrPropertyPath,Vec<AnnotatedTermPath>)> = p:(VerbPath() / VerbSimple()) _ o:ObjectList() _ {
            FocusedTriplePattern {
                focus: (p, o.focus.into_iter().map(AnnotatedTermPath::from).collect()),
                patterns: o.patterns
            }
        }

        //[84]
        rule VerbPath() -> VariableOrPropertyPath = p:Path() {
            p.into()
        }

        //[85]
        rule VerbSimple() -> VariableOrPropertyPath = v:Var() {
            v.into()
        }

        //[86]
        rule ObjectListPath() -> FocusedTripleOrPathPattern<Vec<AnnotatedTermPath>> = o:ObjectListPath_item() **<1,> ("," _) {
            o.into_iter().fold(FocusedTripleOrPathPattern::<Vec<AnnotatedTermPath>>::default(), |mut a, b| {
                a.focus.push(b.focus);
                a.patterns.extend(b.patterns);
                a
            })
        }
        rule ObjectListPath_item() -> FocusedTripleOrPathPattern<AnnotatedTermPath> = o:ObjectPath() _ { o }

        //[87]
        rule ObjectPath() -> FocusedTripleOrPathPattern<AnnotatedTermPath> = g:GraphNodePath() _ a:AnnotationPatternPath()? {
             if let Some(a) = a {
                let mut patterns = g.patterns;
                patterns.extend(a.patterns);
                FocusedTripleOrPathPattern {
                    focus: AnnotatedTermPath {
                        term: g.focus,
                        annotations: a.focus
                    },
                    patterns
                }
            } else {
                FocusedTripleOrPathPattern {
                    focus: AnnotatedTermPath {
                        term: g.focus,
                        annotations: Vec::new()
                    },
                    patterns: g.patterns
                }
            }
        }

        //[88]
        rule Path() -> PropertyPathExpression = PathAlternative()

        //[89]
        rule PathAlternative() -> PropertyPathExpression = p:PathAlternative_item() **<1,> ("|" _) {?
            not_empty_fold(p.into_iter(), |a, b| {
                PropertyPathExpression::Alternative(Box::new(a), Box::new(b))
            })
        }
        rule PathAlternative_item() -> PropertyPathExpression = p:PathSequence() _ { p }

        //[90]
        rule PathSequence() -> PropertyPathExpression = p:PathSequence_item() **<1,> ("/" _) {?
            not_empty_fold(p.into_iter(), |a, b| {
                PropertyPathExpression::Sequence(Box::new(a), Box::new(b))
            })
        }
        rule PathSequence_item() -> PropertyPathExpression = p:PathEltOrInverse() _ { p }

        //[91]
        rule PathElt() -> PropertyPathExpression =
            p:PathPrimary() "?" { PropertyPathExpression::ZeroOrOne(Box::new(p)) } / //TODO: allow space before "?"
            p:PathPrimary() _ "*" { PropertyPathExpression::ZeroOrMore(Box::new(p)) } /
            p:PathPrimary() _ "+" { PropertyPathExpression::OneOrMore(Box::new(p)) } /
            PathPrimary()

        //[92]
        rule PathEltOrInverse() -> PropertyPathExpression =
            "^" _ p:PathElt() { PropertyPathExpression::Reverse(Box::new(p)) } /
            PathElt()

        //[94]
        rule PathPrimary() -> PropertyPathExpression =
            v:iri() { v.into() } /
            "a" { iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type").into() } /
            "!" _ p:PathNegatedPropertySet() { p } /
            "(" _ p:Path() _ ")" { p }

        //[95]
        rule PathNegatedPropertySet() -> PropertyPathExpression =
            "(" _ p:PathNegatedPropertySet_item() **<1,> ("|" _) ")" {
                let mut direct = Vec::new();
                let mut inverse = Vec::new();
                for e in p {
                    match e {
                        Either::Left(a) => direct.push(a),
                        Either::Right(b) => inverse.push(b)
                    }
                }
                if inverse.is_empty() {
                    PropertyPathExpression::NegatedPropertySet(direct)
                } else if direct.is_empty() {
                   PropertyPathExpression::Reverse(Box::new(PropertyPathExpression::NegatedPropertySet(inverse)))
                } else {
                    PropertyPathExpression::Alternative(
                        Box::new(PropertyPathExpression::NegatedPropertySet(direct)),
                        Box::new(PropertyPathExpression::Reverse(Box::new(PropertyPathExpression::NegatedPropertySet(inverse))))
                    )
                }
            } /
            p:PathOneInPropertySet() {
                match p {
                    Either::Left(a) => PropertyPathExpression::NegatedPropertySet(vec![a]),
                    Either::Right(b) => PropertyPathExpression::Reverse(Box::new(PropertyPathExpression::NegatedPropertySet(vec![b]))),
                }
            }
        rule PathNegatedPropertySet_item() -> Either<NamedNode,NamedNode> = p:PathOneInPropertySet() _ { p }

        //[96]
        rule PathOneInPropertySet() -> Either<NamedNode,NamedNode> =
            "^" _ v:iri() { Either::Right(v) } /
            "^" _ "a" { Either::Right(iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type")) } /
            v:iri() { Either::Left(v) } /
            "a" { Either::Left(iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type")) }

        //[98]
        rule TriplesNode() -> FocusedTriplePattern<TermPattern> = Collection() / BlankNodePropertyList()

        //[99]
        rule BlankNodePropertyList() -> FocusedTriplePattern<TermPattern> = "[" _ po:PropertyListNotEmpty() _ "]" {
            let mut patterns = po.patterns;
            let mut bnode = TermPattern::from(bnode());
            for (p, os) in po.focus {
                for o in os {
                    add_to_triple_patterns(bnode.clone(), p.clone(), o, &mut patterns)
                }
            }
            FocusedTriplePattern {
                focus: bnode,
                patterns
            }
        }

        //[100]
        rule TriplesNodePath() -> FocusedTripleOrPathPattern<TermPattern> = CollectionPath() / BlankNodePropertyListPath()

        //[101]
        rule BlankNodePropertyListPath() -> FocusedTripleOrPathPattern<TermPattern> = "[" _ po:PropertyListPathNotEmpty() _ "]" {?
            let mut patterns: Vec<TripleOrPathPattern> = Vec::new();
            let mut bnode = TermPattern::from(bnode());
            for (p, os) in po.focus {
                for o in os {
                    add_to_triple_or_path_patterns(bnode.clone(), p.clone(), o, &mut patterns)?;
                }
            }
            Ok(FocusedTripleOrPathPattern {
                focus: bnode,
                patterns
            })
        }

        //[102]
        rule Collection() -> FocusedTriplePattern<TermPattern> = "(" _ o:Collection_item()+ ")" {
            let mut patterns: Vec<TriplePattern> = Vec::new();
            let mut current_list_node = TermPattern::from(iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#nil"));
            for objWithPatterns in o.into_iter().rev() {
                let new_blank_node = TermPattern::from(bnode());
                patterns.push(TriplePattern::new(new_blank_node.clone(), iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#first"), objWithPatterns.focus.clone()));
                patterns.push(TriplePattern::new(new_blank_node.clone(), iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#rest"), current_list_node));
                current_list_node = new_blank_node;
                patterns.extend_from_slice(&objWithPatterns.patterns);
            }
            FocusedTriplePattern {
                focus: current_list_node,
                patterns
            }
        }
        rule Collection_item() -> FocusedTriplePattern<TermPattern> = o:GraphNode() _ { o }

        //[103]
        rule CollectionPath() -> FocusedTripleOrPathPattern<TermPattern> = "(" _ o:CollectionPath_item()+ _ ")" {
            let mut patterns: Vec<TripleOrPathPattern> = Vec::new();
            let mut current_list_node = TermPattern::from(iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#nil"));
            for objWithPatterns in o.into_iter().rev() {
                let new_blank_node = TermPattern::from(bnode());
                patterns.push(TriplePattern::new(new_blank_node.clone(), iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#first"), objWithPatterns.focus.clone()).into());
                patterns.push(TriplePattern::new(new_blank_node.clone(), iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#rest"), current_list_node).into());
                current_list_node = new_blank_node;
                patterns.extend(objWithPatterns.patterns);
            }
            FocusedTripleOrPathPattern {
                focus: current_list_node,
                patterns
            }
        }
        rule CollectionPath_item() -> FocusedTripleOrPathPattern<TermPattern> = p:GraphNodePath() _ { p }

        //[104]
        rule GraphNode() -> FocusedTriplePattern<TermPattern> =
            t:VarOrTermOrEmbTP() { FocusedTriplePattern::new(t) } /
            TriplesNode()

        //[105]
        rule GraphNodePath() -> FocusedTripleOrPathPattern<TermPattern> =
            t:VarOrTermOrEmbTP() { FocusedTripleOrPathPattern::new(t) } /
            TriplesNodePath()

        //[106]
        rule VarOrTerm() -> TermPattern =
            v:Var() { v.into() } /
            t:GraphTerm() { t.into() }

        //[107]
        rule VarOrIri() -> NamedNodePattern =
            v:Var() { v.into() } /
            i:iri() { i.into() }

        //[108]
        rule Var() -> Variable = name:(VAR1() / VAR2()) { Variable { name: name.into() } }

        //[109]
        rule GraphTerm() -> Term =
            i:iri() { i.into() } /
            l:RDFLiteral() { l.into() } /
            l:NumericLiteral() { l.into() } /
            l:BooleanLiteral() { l.into() } /
            b:BlankNode() { b.into() } /
            NIL() { iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#nil").into() }

        //[110]
        rule Expression() -> Expression = e:ConditionalOrExpression() {e}

        //[111]
        rule ConditionalOrExpression() -> Expression = e:ConditionalOrExpression_item() **<1,> ("||" _) {?
            not_empty_fold(e.into_iter(), |a, b| Expression::Or(Box::new(a), Box::new(b)))
        }
        rule ConditionalOrExpression_item() -> Expression = e:ConditionalAndExpression() _ { e }

        //[112]
        rule ConditionalAndExpression() -> Expression = e:ConditionalAndExpression_item() **<1,> ("&&" _) {?
            not_empty_fold(e.into_iter(), |a, b| Expression::And(Box::new(a), Box::new(b)))
        }
        rule ConditionalAndExpression_item() -> Expression = e:ValueLogical() _ { e }

        //[113]
        rule ValueLogical() -> Expression = RelationalExpression()

        //[114]
        rule RelationalExpression() -> Expression = a:NumericExpression() _ o: RelationalExpression_inner()? { match o {
            Some(("=", Some(b), None)) => Expression::Equal(Box::new(a), Box::new(b)),
            Some(("!=", Some(b), None)) => Expression::Not(Box::new(Expression::Equal(Box::new(a), Box::new(b)))),
            Some((">", Some(b), None)) => Expression::Greater(Box::new(a), Box::new(b)),
            Some((">=", Some(b), None)) => Expression::GreaterOrEqual(Box::new(a), Box::new(b)),
            Some(("<", Some(b), None)) => Expression::Less(Box::new(a), Box::new(b)),
            Some(("<=", Some(b), None)) => Expression::LessOrEqual(Box::new(a), Box::new(b)),
            Some(("IN", None, Some(l))) => Expression::In(Box::new(a), l),
            Some(("NOT IN", None, Some(l))) => Expression::Not(Box::new(Expression::In(Box::new(a), l))),
            Some(_) => unreachable!(),
            None => a
        } }
        rule RelationalExpression_inner() -> (&'input str, Option<Expression>, Option<Vec<Expression>>) =
            s: $("="  / "!=" / ">=" / ">" / "<=" / "<") _ e:NumericExpression() { (s, Some(e), None) } /
            i("IN") _ l:ExpressionList() { ("IN", None, Some(l)) } /
            i("NOT") _ i("IN") _ l:ExpressionList() { ("NOT IN", None, Some(l)) }

        //[115]
        rule NumericExpression() -> Expression = AdditiveExpression()

        //[116]
        rule AdditiveExpression() -> Expression = a:MultiplicativeExpression() _ o:AdditiveExpression_inner()? { match o {
            Some(("+", b)) => Expression::Add(Box::new(a), Box::new(b)),
            Some(("-", b)) => Expression::Subtract(Box::new(a), Box::new(b)),
            Some(_) => unreachable!(),
            None => a,
        } }
        rule AdditiveExpression_inner() -> (&'input str, Expression) = s: $("+" / "-") _ e:AdditiveExpression() {
            (s, e)
        }

        //[117]
        rule MultiplicativeExpression() -> Expression = a:UnaryExpression() _ o: MultiplicativeExpression_inner()? { match o {
            Some(("*", b)) => Expression::Multiply(Box::new(a), Box::new(b)),
            Some(("/", b)) => Expression::Divide(Box::new(a), Box::new(b)),
            Some(_) => unreachable!(),
            None => a
        } }
        rule MultiplicativeExpression_inner() -> (&'input str, Expression) = s: $("*" / "/") _ e:MultiplicativeExpression() {
            (s, e)
        }

        //[118]
        rule UnaryExpression() -> Expression = s: $("!" / "+" / "-")? _ e:PrimaryExpression() { match s {
            Some("!") => Expression::Not(Box::new(e)),
            Some("+") => Expression::UnaryPlus(Box::new(e)),
            Some("-") => Expression::UnaryMinus(Box::new(e)),
            Some(_) => unreachable!(),
            None => e,
        } }

        //[119]
        rule PrimaryExpression() -> Expression =
            BrackettedExpression()  /
            ExprEmbTP() /
            iriOrFunction() /
            v:Var() { v.into() } /
            l:RDFLiteral() { l.into() } /
            l:NumericLiteral() { l.into() } /
            l:BooleanLiteral() { l.into() } /
            BuiltInCall()

        //[120]
        rule BrackettedExpression() -> Expression = "(" _ e:Expression() _ ")" { e }

        //[121]
        rule BuiltInCall() -> Expression =
            a:Aggregate() {? state.new_aggregation(a).map(|v| v.into()) } /
            i("STR") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Str, vec![e]) } /
            i("LANG") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Lang, vec![e]) } /
            i("LANGMATCHES") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::LangMatches, vec![a, b]) } /
            i("DATATYPE") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Datatype, vec![e]) } /
            i("BOUND") _ "(" _ v:Var() _ ")" { Expression::Bound(v) } /
            (i("IRI") / i("URI")) _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Iri, vec![e]) } /
            i("BNODE") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::BNode, vec![e]) } /
            i("BNODE") NIL() { Expression::FunctionCall(Function::BNode, vec![]) }  /
            i("RAND") _ NIL() { Expression::FunctionCall(Function::Rand, vec![]) } /
            i("ABS") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Abs, vec![e]) } /
            i("CEIL") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Ceil, vec![e]) } /
            i("FLOOR") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Floor, vec![e]) } /
            i("ROUND") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Round, vec![e]) } /
            i("CONCAT") e:ExpressionList() { Expression::FunctionCall(Function::Concat, e) } /
            SubstringExpression() /
            i("STRLEN") _ "(" _ e: Expression() _ ")" { Expression::FunctionCall(Function::StrLen, vec![e]) } /
            StrReplaceExpression() /
            i("UCASE") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::UCase, vec![e]) } /
            i("LCASE") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::LCase, vec![e]) } /
            i("ENCODE_FOR_URI") "(" _ e: Expression() _ ")" { Expression::FunctionCall(Function::EncodeForUri, vec![e]) } /
            i("CONTAINS") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::Contains, vec![a, b]) } /
            i("STRSTARTS") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrStarts, vec![a, b]) } /
            i("STRENDS") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrEnds, vec![a, b]) } /
            i("STRBEFORE") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrBefore, vec![a, b]) } /
            i("STRAFTER") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrAfter, vec![a, b]) } /
            i("YEAR") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Year, vec![e]) } /
            i("MONTH") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Month, vec![e]) } /
            i("DAY") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Day, vec![e]) } /
            i("HOURS") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Hours, vec![e]) } /
            i("MINUTES") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Minutes, vec![e]) } /
            i("SECONDS") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Seconds, vec![e]) } /
            i("TIMEZONE") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Timezone, vec![e]) } /
            i("TZ") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Tz, vec![e]) } /
            i("NOW") _ NIL() { Expression::FunctionCall(Function::Now, vec![]) } /
            i("UUID") _ NIL() { Expression::FunctionCall(Function::Uuid, vec![]) }/
            i("STRUUID") _ NIL() { Expression::FunctionCall(Function::StrUuid, vec![]) } /
            i("MD5") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Md5, vec![e]) } /
            i("SHA1") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Sha1, vec![e]) } /
            i("SHA256") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Sha256, vec![e]) } /
            i("SHA384") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Sha384, vec![e]) } /
            i("SHA512") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Sha512, vec![e]) } /
            i("COALESCE") e:ExpressionList() { Expression::Coalesce(e) } /
            i("IF") _ "(" _ a:Expression() _ "," _ b:Expression() _ "," _ c:Expression() _ ")" { Expression::If(Box::new(a), Box::new(b), Box::new(c)) } /
            i("STRLANG") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrLang, vec![a, b]) }  /
            i("STRDT") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrDt, vec![a, b]) } /
            i("sameTerm") "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::SameTerm(Box::new(a), Box::new(b)) } /
            (i("isIRI") / i("isURI")) _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::IsIri, vec![e]) } /
            i("isBLANK") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::IsBlank, vec![e]) } /
            i("isLITERAL") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::IsLiteral, vec![e]) } /
            i("isNUMERIC") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::IsNumeric, vec![e]) } /
            RegexExpression() /
            ExistsFunc() /
            NotExistsFunc() /
            i("TRIPLE") "(" _ s:Expression() _ "," _ p:Expression() "," _ o:Expression() ")" { Expression::FunctionCall(Function::Triple, vec![s, p, o]) } /
            i("SUBJECT") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Subject, vec![e]) } /
            i("PREDICATE") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Predicate, vec![e]) } /
            i("OBJECT") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Object, vec![e]) } /
            i("isTriple") "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::IsTriple, vec![e]) }

        //[122]
        rule RegexExpression() -> Expression =
            i("REGEX") _ "(" _ a:Expression() _ "," _ b:Expression() _ "," _ c:Expression() _ ")" { Expression::FunctionCall(Function::Regex, vec![a, b, c]) } /
            i("REGEX") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::Regex, vec![a, b]) }


        rule SubstringExpression() -> Expression =
            i("SUBSTR") _ "(" _ a:Expression() _ "," _ b:Expression() _ "," _ c:Expression() _ ")" { Expression::FunctionCall(Function::SubStr, vec![a, b, c]) } /
            i("SUBSTR") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::SubStr, vec![a, b]) }


        //[124]
        rule StrReplaceExpression() -> Expression =
            i("REPLACE") _ "(" _ a:Expression() _ "," _ b:Expression() _ "," _ c:Expression() _ "," _ d:Expression() _ ")" { Expression::FunctionCall(Function::Replace, vec![a, b, c, d]) } /
            i("REPLACE") _ "(" _ a:Expression() _ "," _ b:Expression() _ "," _ c:Expression() _ ")" { Expression::FunctionCall(Function::Replace, vec![a, b, c]) }

        //[125]
        rule ExistsFunc() -> Expression = i("EXISTS") _ p:GroupGraphPattern() { Expression::Exists(Box::new(p)) }

        //[126]
        rule NotExistsFunc() -> Expression = i("NOT") _ i("EXISTS") _ p:GroupGraphPattern() { Expression::Not(Box::new(Expression::Exists(Box::new(p)))) }

        //[127]
        rule Aggregate() -> AggregationFunction =
            i("COUNT") _ "(" _ i("DISTINCT") _ "*" _ ")" { AggregationFunction::Count { expr: None, distinct: true } } /
            i("COUNT") _ "(" _ i("DISTINCT") _ e:Expression() _ ")" { AggregationFunction::Count { expr: Some(Box::new(e)), distinct: true } } /
            i("COUNT") _ "(" _ "*" _ ")" { AggregationFunction::Count { expr: None, distinct: false } } /
            i("COUNT") _ "(" _ e:Expression() _ ")" { AggregationFunction::Count { expr: Some(Box::new(e)), distinct: false } } /
            i("SUM") _ "(" _ i("DISTINCT") _ e:Expression() _ ")" { AggregationFunction::Sum { expr: Box::new(e), distinct: true } } /
            i("SUM") _ "(" _ e:Expression() _ ")" { AggregationFunction::Sum { expr: Box::new(e), distinct: false } } /
            i("MIN") _ "(" _ i("DISTINCT") _ e:Expression() _ ")" { AggregationFunction::Min { expr: Box::new(e), distinct: true } } /
            i("MIN") _ "(" _ e:Expression() _ ")" { AggregationFunction::Min { expr: Box::new(e), distinct: false } } /
            i("MAX") _ "(" _ i("DISTINCT") _ e:Expression() _ ")" { AggregationFunction::Max { expr: Box::new(e), distinct: true } } /
            i("MAX") _ "(" _ e:Expression() _ ")" { AggregationFunction::Max { expr: Box::new(e), distinct: false } } /
            i("AVG") _ "(" _ i("DISTINCT") _ e:Expression() _ ")" { AggregationFunction::Avg { expr: Box::new(e), distinct: true } } /
            i("AVG") _ "(" _ e:Expression() _ ")" { AggregationFunction::Avg { expr: Box::new(e), distinct: false } } /
            i("SAMPLE") _ "(" _ i("DISTINCT") _ e:Expression() _ ")" { AggregationFunction::Sample { expr: Box::new(e), distinct: true } } /
            i("SAMPLE") _ "(" _ e:Expression() _ ")" { AggregationFunction::Sample { expr: Box::new(e), distinct: false } } /
            i("GROUP_CONCAT") _ "(" _ i("DISTINCT") _ e:Expression() _ ";" _ i("SEPARATOR") _ "=" _ s:String() _ ")" { AggregationFunction::GroupConcat { expr: Box::new(e), distinct: true, separator: Some(s) } } /
            i("GROUP_CONCAT") _ "(" _ i("DISTINCT") _ e:Expression() _ ")" { AggregationFunction::GroupConcat { expr: Box::new(e), distinct: true, separator: None } } /
            i("GROUP_CONCAT") _ "(" _ e:Expression() _ ";" _ i("SEPARATOR") _ "=" _ s:String() _ ")" { AggregationFunction::GroupConcat { expr: Box::new(e), distinct: true, separator: Some(s) } } /
            i("GROUP_CONCAT") _ "(" _ e:Expression() _ ")" { AggregationFunction::GroupConcat { expr: Box::new(e), distinct: false, separator: None } } /
            name:iri() _ "(" _ i("DISTINCT") _ e:Expression() _ ")" { AggregationFunction::Custom { name, expr: Box::new(e), distinct: true } } /
            name:iri() _ "(" _ e:Expression() _ ")" { AggregationFunction::Custom { name, expr: Box::new(e), distinct: false } }

        //[128]
        rule iriOrFunction() -> Expression = i: iri() _ a: ArgList()? {
            match a {
                Some(a) => Expression::FunctionCall(Function::Custom(i), a),
                None => i.into()
            }
        }

        //[129]
        rule RDFLiteral() -> Literal =
            value:String() _ "^^" _ datatype:iri() { Literal::Typed { value, datatype } } /
            value:String() _ language:LANGTAG() { Literal::LanguageTaggedString { value, language: language.into_inner() } } /
            value:String() { Literal::Simple { value } }

        //[130]
        rule NumericLiteral() -> Literal  = NumericLiteralUnsigned() / NumericLiteralPositive() / NumericLiteralNegative()

        //[131]
        rule NumericLiteralUnsigned() -> Literal =
            d:$(DOUBLE()) { Literal::Typed { value: d.into(), datatype: iri("http://www.w3.org/2001/XMLSchema#double") } } /
            d:$(DECIMAL()) { Literal::Typed { value: d.into(), datatype: iri("http://www.w3.org/2001/XMLSchema#decimal") } } /
            i:$(INTEGER()) { Literal::Typed { value: i.into(), datatype: iri("http://www.w3.org/2001/XMLSchema#integer") } }

        //[132]
        rule NumericLiteralPositive() -> Literal =
            d:$(DOUBLE_POSITIVE()) { Literal::Typed { value: d.into(), datatype: iri("http://www.w3.org/2001/XMLSchema#double") } } /
            d:$(DECIMAL_POSITIVE()) { Literal::Typed { value: d.into(), datatype: iri("http://www.w3.org/2001/XMLSchema#decimal") } } /
            i:$(INTEGER_POSITIVE()) { Literal::Typed { value: i.into(), datatype: iri("http://www.w3.org/2001/XMLSchema#integer") } }


        //[133]
        rule NumericLiteralNegative() -> Literal =
            d:$(DOUBLE_NEGATIVE()) { Literal::Typed { value: d.into(), datatype: iri("http://www.w3.org/2001/XMLSchema#double") } } /
            d:$(DECIMAL_NEGATIVE()) { Literal::Typed { value: d.into(), datatype: iri("http://www.w3.org/2001/XMLSchema#decimal") } } /
            i:$(INTEGER_NEGATIVE()) { Literal::Typed { value: i.into(), datatype: iri("http://www.w3.org/2001/XMLSchema#integer") } }

        //[134]
        rule BooleanLiteral() -> Literal =
            "true" { Literal::Typed { value: "true".into(), datatype: iri("http://www.w3.org/2001/XMLSchema#boolean") } } /
            "false" { Literal::Typed { value: "false".into(), datatype: iri("http://www.w3.org/2001/XMLSchema#boolean") } }

        //[135]
        rule String() -> String = STRING_LITERAL_LONG1() / STRING_LITERAL_LONG2() / STRING_LITERAL1() / STRING_LITERAL2()

        //[136]
        rule iri() -> NamedNode = i:(IRIREF() / PrefixedName()) {
            iri(i.into_inner())
        }

        //[137]
        rule PrefixedName() -> Iri<String> = PNAME_LN() /
            ns:PNAME_NS() {? if let Some(iri) = state.namespaces.get(ns).cloned() {
                Iri::parse(iri).map_err(|_| "IRI parsing failed")
            } else {
                Err("Prefix not found")
            } }

        //[138]
        rule BlankNode() -> BlankNode = id:BLANK_NODE_LABEL() {?
            let node = BlankNode { id: id.to_owned() };
            if state.used_bnodes.contains(&node) {
                Err("Already used blank node id")
            } else {
                state.currently_used_bnodes.insert(node.clone());
                Ok(node)
            }
        } / ANON() { bnode() }

        //[139]
        rule IRIREF() -> Iri<String> = "<" i:$((!['>'] [_])*) ">" {?
            state.parse_iri(i).map_err(|_| "IRI parsing failed")
        }

        //[140]
        rule PNAME_NS() -> &'input str = ns:$(PN_PREFIX()?) ":" {
            ns
        }

        //[141]
        rule PNAME_LN() -> Iri<String> = ns:PNAME_NS() local:$(PN_LOCAL()) {?
            if let Some(base) = state.namespaces.get(ns) {
                let mut iri = base.clone();
                iri.push_str(&unescape_pn_local(local));
                Iri::parse(iri).map_err(|_| "IRI parsing failed")
            } else {
                Err("Prefix not found")
            }
        }

        //[142]
        rule BLANK_NODE_LABEL() -> &'input str = "_:" b:$((['0'..='9'] / PN_CHARS_U()) PN_CHARS()* ("."+ PN_CHARS()+)*) {
            b
        }

        //[143]
        rule VAR1() -> &'input str = "?" v:$(VARNAME()) { v }

        //[144]
        rule VAR2() -> &'input str = "$" v:$(VARNAME()) { v }

        //[145]
        rule LANGTAG() -> LanguageTag<String> = "@" l:$(['a' ..= 'z' | 'A' ..= 'Z']+ ("-" ['a' ..= 'z' | 'A' ..= 'Z' | '0' ..= '9']+)*) {?
            LanguageTag::parse(l.to_ascii_lowercase()).map_err(|_| "language tag parsing failed")
        }

        //[146]
        rule INTEGER() = ['0'..='9']+ {}

        //[147]
        rule DECIMAL() = (['0'..='9']+ "." ['0'..='9']* / ['0'..='9']* "." ['0'..='9']+)

        //[148]
        rule DOUBLE() = (['0'..='9']+ "." ['0'..='9']* / "." ['0'..='9']+ / ['0'..='9']+) EXPONENT()

        //[149]
        rule INTEGER_POSITIVE() = "+" _ INTEGER()

        //[150]
        rule DECIMAL_POSITIVE() = "+" _ DECIMAL()

        //[151]
        rule DOUBLE_POSITIVE() = "+" _ DOUBLE()

        //[152]
        rule INTEGER_NEGATIVE() = "-" _ INTEGER()

        //[153]
        rule DECIMAL_NEGATIVE() = "-" _ DECIMAL()

        //[154]
        rule DOUBLE_NEGATIVE() = "-" _ DOUBLE()

        //[155]
        rule EXPONENT() = [eE] ['+' | '-']? ['0'..='9']+

        //[156]
        rule STRING_LITERAL1() -> String = "'" l:$((STRING_LITERAL1_simple_char() / ECHAR())*) "'" {
            unescape_echars(l).to_string()
        }
        rule STRING_LITERAL1_simple_char() = !['\u{27}' | '\u{5C}' | '\u{A}' | '\u{D}'] [_]


        //[157]
        rule STRING_LITERAL2() -> String = "\"" l:$((STRING_LITERAL2_simple_char() / ECHAR())*) "\"" {
            unescape_echars(l).to_string()
        }
        rule STRING_LITERAL2_simple_char() = !['\u{22}' | '\u{5C}' | '\u{A}' | '\u{D}'] [_]

        //[158]
        rule STRING_LITERAL_LONG1() -> String = "'''" l:$(STRING_LITERAL_LONG1_inner()*) "'''" {
            unescape_echars(l).to_string()
        }
        rule STRING_LITERAL_LONG1_inner() = ("''" / "'")? (STRING_LITERAL_LONG1_simple_char() / ECHAR())
        rule STRING_LITERAL_LONG1_simple_char() = !['\'' | '\\'] [_]

        //[159]
        rule STRING_LITERAL_LONG2() -> String = "\"\"\"" l:$(STRING_LITERAL_LONG2_inner()*) "\"\"\"" {
            unescape_echars(l).to_string()
        }
        rule STRING_LITERAL_LONG2_inner() = ("\"\"" / "\"")? (STRING_LITERAL_LONG2_simple_char() / ECHAR())
        rule STRING_LITERAL_LONG2_simple_char() = !['"' | '\\'] [_]

        //[160]
        rule ECHAR() = "\\" ['t' | 'b' | 'n' | 'r' | 'f' | '"' |'\'' | '\\']

        //[161]
        rule NIL() = "(" WS()* ")"

        //[162]
        rule WS() = quiet! { ['\u{20}' | '\u{9}' | '\u{D}' | '\u{A}'] }

        //[163]
        rule ANON() = "[" WS()* "]"

        //[164]
        rule PN_CHARS_BASE() = ['A' ..= 'Z' | 'a' ..= 'z' | '\u{00C0}' ..='\u{00D6}' | '\u{00D8}'..='\u{00F6}' | '\u{00F8}'..='\u{02FF}' | '\u{0370}'..='\u{037D}' | '\u{037F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}']

        //[165]
        rule PN_CHARS_U() = ['_'] / PN_CHARS_BASE()

        //[166]
        rule VARNAME() = (['0'..='9'] / PN_CHARS_U()) (['0' ..= '9' | '\u{00B7}' | '\u{0300}'..='\u{036F}' | '\u{203F}'..='\u{2040}'] / PN_CHARS_U())*

        //[167]
        rule PN_CHARS() = ['-' | '0' ..= '9' | '\u{00B7}' | '\u{0300}'..='\u{036F}' | '\u{203F}'..='\u{2040}'] / PN_CHARS_U()

        //[168]
        rule PN_PREFIX() = PN_CHARS_BASE() PN_CHARS()* ("."+ PN_CHARS()+)*

        //[169]
        rule PN_LOCAL() = (PN_CHARS_U() / [':' | '0'..='9'] / PLX()) (PN_CHARS() / [':'] / PLX())* (['.']+ (PN_CHARS() / [':'] / PLX())+)?

        //[170]
        rule PLX() = PERCENT() / PN_LOCAL_ESC()

        //[171]
        rule PERCENT() = ['%'] HEX() HEX()

        //[172]
        rule HEX() = ['0' ..= '9' | 'A' ..= 'F' | 'a' ..= 'f']

        //[173]
        rule PN_LOCAL_ESC() = ['\\'] ['_' | '~' | '.' | '-' | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '=' | '/' | '?' | '#' | '@' | '%'] //TODO: added '/' to make tests pass but is it valid?

        //[174]
        rule EmbTP() -> TriplePattern = "<<" _ s:EmbSubjectOrObject() _ p:Verb() _ o:EmbSubjectOrObject() _ ">>" {
            TriplePattern { subject: s, predicate: p, object: o }
        }

        //[175]
        rule EmbTriple() -> GroundTriple = "<<" _ s:DataValueTerm() _ p:EmbTriple_p() _ o:DataValueTerm() _ ">>" {?
            Ok(GroundTriple {
                subject: s.try_into().map_err(|_| "Literals are not allowed in subject position of nested patterns")?,
                predicate: p,
                object: o
            })
        }
        rule EmbTriple_p() -> NamedNode = i: iri() { i } / "a" { iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type") }

        //[176]
        rule EmbSubjectOrObject() -> TermPattern =
            t:EmbTP() { t.into() } /
            v:Var() { v.into() } /
            b:BlankNode() { b.into() } /
            i:iri() { i.into() } /
            l:RDFLiteral() { l.into() } /
            l:NumericLiteral() { l.into() } /
            l:BooleanLiteral() { l.into() }

        //[177]
        rule DataValueTerm() -> GroundTerm = i:iri() { i.into() } /
            l:RDFLiteral() { l.into() } /
            l:NumericLiteral() { l.into() } /
            l:BooleanLiteral() { l.into() } /
            t:EmbTriple() { t.into() }

        //[178]
        rule VarOrTermOrEmbTP() -> TermPattern =
            t:EmbTP() { t.into() } /
            v:Var() { v.into() } /
            t:GraphTerm() { t.into() }

        //[179]
        rule AnnotationPattern() -> FocusedTriplePattern<Vec<(NamedNodePattern,Vec<AnnotatedTerm>)>> = "{|" _ a:PropertyListNotEmpty() _ "|}" { a }

        //[180]
        rule AnnotationPatternPath() -> FocusedTripleOrPathPattern<Vec<(VariableOrPropertyPath,Vec<AnnotatedTermPath>)>> = "{|" _ a: PropertyListPathNotEmpty() _ "|}" { a }

        //[181]
        rule ExprEmbTP() -> Expression = "<<" _ s:ExprVarOrTerm() _ p:Verb() _ o:ExprVarOrTerm() _ ">>" {
            Expression::FunctionCall(Function::Triple, vec![s, p.into(), o])
        }

        //[182]
        rule ExprVarOrTerm() -> Expression =
            ExprEmbTP() /
            i:iri() { i.into() } /
            l:RDFLiteral() { l.into() } /
            l:NumericLiteral() { l.into() } /
            l:BooleanLiteral() { l.into() } /
            v:Var() { v.into() }

        //space
        rule _() = quiet! { ([' ' | '\t' | '\n' | '\r'] / comment())* }

        //comment
        rule comment() = quiet! { ['#'] (!['\r' | '\n'] [_])* }

        rule i(literal: &'static str) = input: $([_]*<{literal.len()}>) {?
            if input.eq_ignore_ascii_case(literal) {
                Ok(())
            } else {
                Err(literal)
            }
        }
    }
}
