/// Simple fixed-width table printer. No external dep.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(headers: Vec<&str>) -> Self {
        Self {
            headers: headers.iter().map(|s| s.to_string()).collect(),
            rows: vec![],
        }
    }

    pub fn add_row(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    pub fn print(&self) {
        let col_count = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.len()).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                if i < col_count {
                    widths[i] = widths[i].max(cell.len());
                }
            }
        }

        let header_line = self
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| format!("{:<width$}", h, width = widths[i]))
            .collect::<Vec<_>>()
            .join("  ");

        let sep_line = widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  ");

        println!("{}", header_line);
        println!("{}", sep_line);

        if self.rows.is_empty() {
            println!("(none)");
            return;
        }

        for row in &self.rows {
            let line = row
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    let width = widths.get(i).copied().unwrap_or(cell.len());
                    format!("{:<width$}", cell, width = width)
                })
                .collect::<Vec<_>>()
                .join("  ");
            println!("{}", line);
        }
    }
}

pub fn em_dash() -> &'static str {
    "—"
}

pub fn print_kv(key: &str, value: &str) {
    println!("{:<20} {}", format!("{}:", key), value);
}

pub fn print_section(title: &str) {
    println!("\n{}", title);
    println!("{}", "─".repeat(title.len()));
}
