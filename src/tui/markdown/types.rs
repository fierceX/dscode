#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum MdBlock {
    Blank,
    Paragraph(Vec<InlineNode>),
    Heading {
        level: usize,
        content: Vec<InlineNode>,
    },
    BlockQuote(Vec<InlineNode>),
    ListItem {
        marker: String,
        content: Vec<InlineNode>,
    },
    CodeBlock {
        lang: Option<String>,
        lines: Vec<String>,
    },
    Table(TableRows),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum InlineNode {
    Text(String),
    Code(String),
    Strong(Vec<InlineNode>),
    Emphasis(Vec<InlineNode>),
    Link { text: Vec<InlineNode>, href: String },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TableAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct TableRows {
    pub(crate) header: Vec<String>,
    pub(crate) alignments: Vec<TableAlign>,
    pub(crate) rows: Vec<Vec<String>>,
}
