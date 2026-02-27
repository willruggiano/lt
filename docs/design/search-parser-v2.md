Since you have the GraphQL schema at compile time and you are targeting a user-facing search bar, this completely solidifies your architectural path. You will want to use a **`build.rs` (build script)** to bridge the gap between your schema and your parser.

When you use `build.rs`, you have two primary ways to generate a parser. You can either generate a **grammar file** (which a library then compiles into Rust), or you can generate **Rust source code** directly.

Here is exactly how to execute this based on your constraints, using the SOTA tools:

### Option 1: The Pest Route (Easiest to Generate)

Because **Pest** relies on a standalone `.pest` file to define its PEG (Parsing Expression Grammar), it is remarkably easy to generate programmatically.

**The Pipeline:**

1. Your `build.rs` uses `apollo-compiler` or `graphql-parser` to parse your `schema.graphql`.
2. It extracts the arguments for your search query (e.g., `assignee`, `author`, `status`).
3. It creates a string formatted as a Pest grammar and writes it to `OUT_DIR/search.pest`.
4. Your main Rust code uses `pest_derive` to include that generated file.

**What your `build.rs` would generate:**

```pest
// generated_search.pest
search_query = { filter ~ (WHITESPACE+ ~ filter)* }
filter       = { field ~ ":" ~ value }

// Your build.rs dynamically populates this specific line!
field        = { "assignee" | "author" | "status" | "label" }

value        = { (ASCII_ALPHANUMERIC | "-" | "_")+ }
WHITESPACE   = _{ " " | "\t" }

```

- **Pros:** Generating a text file containing PEG syntax is vastly easier than generating valid Rust code.
- **Cons:** Pest's default error messages are decent, but they aren't "IDE-quality" out of the box without some manual formatting work.

---

### Option 2: The Chumsky Route (Best User Experience)

Since this is a user-facing search bar, error recovery is a massive priority. If a user types `assig:me`, you want the parser to know that `assig` is a field token and suggest, _"Unknown field 'assig'. Did you mean 'assignee'?"_ **Chumsky** is the SOTA for this, but because it is a parser combinator, you cannot just generate a grammar file. Your `build.rs` must generate the actual Rust source code for the parser.

**The Pipeline:**

1. Your `build.rs` parses the `schema.graphql`.
2. It uses a templating engine (like `askama` or `tinytemplate`) or the `quote` crate to generate a `.rs` file containing Chumsky parser combinators.
3. It writes this to `OUT_DIR/generated_parser.rs`.
4. Your main code includes this generated module via `include!(concat!(env!("OUT_DIR"), "/generated_parser.rs"));`.

**What your `build.rs` would generate:**

```rust
// generated_parser.rs
use chumsky::prelude::*;

#[derive(Debug, Clone, PartialEq)]
pub enum SearchField {
    // Your build.rs dynamically generates these enum variants
    Assignee,
    Author,
    Status,
}

pub fn search_parser<'a>() -> impl Parser<'a, &'a str, Vec<(SearchField, String)>, extra::Err<Rich<'a, char>>> {
    // Your build.rs dynamically generates this matching logic
    let field = choice((
        text::keyword("assignee").to(SearchField::Assignee),
        text::keyword("author").to(SearchField::Author),
        text::keyword("status").to(SearchField::Status),
    ));

    let value = text::ident().map(String::from); // Simplified for example

    let filter = field.then_ignore(just(':')).then(value);

    filter.padded().repeated().collect()
}

```

- **Pros:** World-class, rich error messages (perfect for webapps or CLIs). High performance. Type-safe enum generation.
- **Cons:** Writing a build script that generates Rust code is more complex and fragile than generating a `.pest` text file.

### The Verdict

If you want to get this working quickly and cleanly, go with **Pest**. The build script is trivial to write, and PEG grammars map beautifully to search syntax.

If your primary goal is building an incredible developer/user experience with rich syntax error reporting in the UI, bite the bullet and use `quote` in your `build.rs` to generate a **Chumsky** parser.
