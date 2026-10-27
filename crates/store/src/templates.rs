//! Spark's templates: emails to start from, kept on this computer. Each has
//! a name, a subject (it can be empty) and a rich-text body that may hold
//! placeholders, which the composer fills in. Applying one marks it used,
//! for Spark's Recent section.

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::{Result, Store};

/// Version 20: templates, in the user's order.
pub(crate) const SCHEMA_TEMPLATES: &str = "
CREATE TABLE templates (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    subject TEXT NOT NULL,
    html TEXT NOT NULL,
    -- The body as plain text, to show under the name.
    text TEXT NOT NULL,
    position INTEGER NOT NULL,
    used_at INTEGER
);
";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: i64,
    pub name: String,
    pub subject: String,
    /// Sanitized by the mail crate before it's stored.
    pub html: String,
    pub text: String,
    /// When it was last applied (Unix ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_at: Option<i64>,
}

/// A template as it's saved.
#[derive(Clone, Copy, Debug)]
pub struct NewTemplate<'a> {
    pub name: &'a str,
    pub subject: &'a str,
    pub html: &'a str,
    pub text: &'a str,
}

const COLUMNS: &str = "id, name, subject, html, text, used_at";

fn template_row(r: &Row) -> Result<Template> {
    Ok(Template {
        id: r.get(0)?,
        name: r.get(1)?,
        subject: r.get(2)?,
        html: r.get(3)?,
        text: r.get(4)?,
        used_at: r.get(5)?,
    })
}

impl Store {
    /// In the user's order.
    pub fn templates(&self) -> Result<Vec<Template>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM templates ORDER BY position, id"
        ))?;
        stmt.query_map([], template_row)?.collect()
    }

    pub fn template(&self, id: i64) -> Result<Option<Template>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM templates WHERE id = ?1"),
                [id],
                template_row,
            )
            .optional()
    }

    /// A new template, last in the list.
    pub fn add_template(&self, new: NewTemplate) -> Result<Template> {
        self.conn.execute(
            "INSERT INTO templates (name, subject, html, text, position)
             VALUES (?1, ?2, ?3, ?4, (SELECT COALESCE(MAX(position), 0) + 1 FROM templates))",
            params![new.name, new.subject, new.html, new.text],
        )?;
        Ok(Template {
            id: self.conn.last_insert_rowid(),
            name: new.name.to_owned(),
            subject: new.subject.to_owned(),
            html: new.html.to_owned(),
            text: new.text.to_owned(),
            used_at: None,
        })
    }

    /// False if it's gone.
    pub fn update_template(&self, id: i64, new: NewTemplate) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE templates SET name = ?2, subject = ?3, html = ?4, text = ?5 WHERE id = ?1",
            params![id, new.name, new.subject, new.html, new.text],
        )?;
        Ok(changed > 0)
    }

    pub fn remove_template(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM templates WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Puts templates in the order of `ids`; ones left out keep their place after them.
    pub fn order_templates(&mut self, ids: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE templates SET position = position + ?1",
            [ids.len() as i64],
        )?;
        for (position, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE templates SET position = ?2 WHERE id = ?1",
                params![id, position as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Notes that a template was applied, for the Recent section.
    pub fn template_used(&self, id: i64, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE templates SET used_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }
}
