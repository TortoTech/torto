//! The image recognizer supplies a bounded XHTML table, never browser content.
use super::*;

pub fn parse_recognized_table(html: &str) -> Result<TableBlock, String> {
    if html.len() > 128 * 1024 || html.contains('\u{fffd}') {
        return Err("Oversized or damaged recognized table".into());
    }
    let document = Document::parse(html).map_err(|e| e.to_string())?;
    let root = document.root_element();
    if !root.has_tag_name("table") {
        return Err("Expected one table".into());
    }
    let mut cells = 0;
    for node in root.descendants().filter(Node::is_element) {
        let name = node.tag_name().name();
        if !matches!(
            name,
            "table"
                | "thead"
                | "tbody"
                | "tfoot"
                | "tr"
                | "td"
                | "th"
                | "caption"
                | "br"
                | "span"
                | "p"
                | "sup"
                | "sub"
                | "em"
                | "strong"
                | "i"
                | "b"
        ) || name == "table" && node != root
            || node
                .attributes()
                .any(|a| !matches!(a.name(), "rowspan" | "colspan" | "scope"))
        {
            return Err("Unsupported recognized table structure".into());
        }
        if node != root {
            let parent = node.parent_element().ok_or("Missing table parent")?;
            let valid = match parent.tag_name().name() {
                "table" => matches!(name, "thead" | "tbody" | "tfoot" | "tr" | "caption"),
                "thead" | "tbody" | "tfoot" => name == "tr",
                "tr" => matches!(name, "td" | "th"),
                _ => matches!(
                    name,
                    "br" | "span" | "p" | "sup" | "sub" | "em" | "strong" | "i" | "b"
                ),
            };
            if !valid {
                return Err("Invalid table element placement".into());
            }
        }
        if matches!(name, "table" | "thead" | "tbody" | "tfoot" | "tr")
            && node
                .children()
                .filter(Node::is_text)
                .any(|child| child.text().is_some_and(|text| !text.trim().is_empty()))
        {
            return Err("Text outside table cells".into());
        }
        if matches!(name, "td" | "th") {
            cells += 1;
        }
        for attr in ["rowspan", "colspan"] {
            if node
                .attribute(attr)
                .is_some_and(|v| v.parse::<u16>().map_or(true, |v| v == 0 || v > 256))
            {
                return Err("Invalid table span".into());
            }
        }
    }
    if cells == 0 || cells > 8192 {
        return Err("Invalid table cell count".into());
    }
    let descriptor = SpineItem {
        id: SpineItemId::new("image-table").unwrap(),
        href: PublicationUrl::parse("image-table.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    let section = parse_section(
        &format!("<html><body>{html}</body></html>"),
        &descriptor,
        |_| None,
    )
    .map_err(|e| e.to_string())?;
    let mut blocks = section.blocks.into_iter();
    let Some(Block::Table(table)) = blocks.next() else {
        return Err("Expected a data table".into());
    };
    if blocks.next().is_some() || table.rows.is_empty() || table.rows.len() > 256 {
        return Err("Invalid table body".into());
    }
    validate_recognized_table(&table)?;
    Ok(table)
}

pub fn validate_recognized_table(table: &TableBlock) -> Result<(), String> {
    if table.rows.is_empty()
        || table.rows.len() > 256
        || table.rows.iter().map(|r| r.cells.len()).sum::<usize>() > 8192
    {
        return Err("Invalid table body".into());
    }
    let mut occupied = vec![vec![false; 48]; table.rows.len()];
    let mut width = 0;
    for (row, item) in table.rows.iter().enumerate() {
        let mut col = 0;
        for cell in &item.cells {
            if cell.column_span == 0 || cell.row_span == 0 {
                return Err("Invalid table span".into());
            }
            while col < 48 && occupied[row][col] {
                col += 1;
            }
            let end_col = col + usize::from(cell.column_span);
            let end_row = row + usize::from(cell.row_span);
            if end_col > 48 || end_row > table.rows.len() {
                return Err("Table span exceeds grid".into());
            }
            for slots in &mut occupied[row..end_row] {
                for slot in &mut slots[col..end_col] {
                    if *slot {
                        return Err("Overlapping table cells".into());
                    }
                    *slot = true;
                }
            }
            width = width.max(end_col);
            col = end_col;
        }
    }
    if occupied.iter().any(|row| row[..width].contains(&false)) {
        return Err("Incomplete table grid".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merged_headers_are_preserved_and_malformed_grids_are_rejected() {
        let table = parse_recognized_table("<table><caption>Table 1</caption><thead><tr><th rowspan='2'>Category</th><th colspan='2'>Value</th></tr><tr><th>2023</th><th>2024</th></tr></thead><tbody><tr><td>Example</td><td>-1.2</td><td>3.4%</td></tr></tbody></table>").unwrap();
        assert_eq!(table.rows.len(), 3);
        assert_eq!(table.rows[0].cells[0].row_span, 2);
        assert_eq!(table.rows[0].cells[1].column_span, 2);
        assert!(
            parse_recognized_table("<table><tr><td rowspan='3'>A</td><td>B</td></tr></table>")
                .is_err()
        );
        assert!(
            parse_recognized_table(
                "<table><tr><td>A</td><td>B</td></tr><tr><td>C</td></tr></table>"
            )
            .is_err()
        );
        assert!(
            parse_recognized_table("<table><tr><td><img src='remote'/></td></tr></table>").is_err()
        );
    }
}
