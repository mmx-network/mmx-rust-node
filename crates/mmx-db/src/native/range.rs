use super::*;
use crate::{
    scan::{self, Item, Payload, Source},
    ReadBudget,
};
use std::ops::Bound;

struct MemoryCursor<'a> {
    mem: &'a Memtable,
    table: String,
    next: Bound<OrderKey>,
    end: Option<Key>,
    max: u32,
    work: scan::Work,
}
impl<'a> Iterator for MemoryCursor<'a> {
    type Item = Result<Item<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let ((table, key, Reverse(version)), value) = self
                .mem
                .range((self.next.clone(), Bound::Unbounded))
                .next()?;
            if *table != self.table || self.end.as_ref().is_some_and(|end| key >= end) {
                return None;
            }
            if let Err(error) = self.work.charge() {
                return Some(Err(error));
            }
            if let Err(error) = self.work.charge_bytes(table.len() + key.0.len()) {
                return Some(Err(error));
            }
            self.next = Bound::Excluded((table.clone(), key.clone(), Reverse(0)));
            let (version, value) = if *version > self.max {
                let ((t, k, Reverse(v)), value) = self
                    .mem
                    .range((table.clone(), key.clone(), Reverse(self.max))..)
                    .next()?;
                if t != table || k != key {
                    continue;
                }
                (*v, *value)
            } else {
                (*version, *value)
            };
            return Some(Ok(Item {
                key: key.clone(),
                version,
                value: value.map(Payload::Reference),
            }));
        }
    }
}
impl State {
    fn range_sources(
        &self,
        table: &str,
        start: &[u8],
        end: Option<&[u8]>,
        max: u32,
        work: scan::Work,
    ) -> Result<Vec<Source<'_>>> {
        let mut sources: Vec<Source<'_>> = Vec::new();
        for view in &self.runs {
            if view.cutoff == 0 {
                continue;
            }
            let entries = view.file.run.visible_entries_range_counted(
                table,
                start,
                end,
                max.min(view.cutoff - 1),
                work.clone(),
            )?;
            sources.push(Box::new(entries.map(|entry| {
                let entry = entry?;
                Ok(Item {
                    key: Key(entry.write.key),
                    version: entry.version,
                    value: entry
                        .write
                        .value
                        .map(|v| ValueRef::decode(&v))
                        .transpose()?
                        .map(Payload::Reference),
                })
            })));
        }
        sources.push(Box::new(MemoryCursor {
            mem: &self.mem,
            table: table.to_owned(),
            next: Bound::Included((table.to_owned(), Key(start.to_vec()), Reverse(max))),
            end: end.map(|v| Key(v.to_vec())),
            max,
            work,
        }));
        Ok(sources)
    }
}
impl Native {
    /// Streaming range merge. The same budget can cover multiple queries.
    pub fn scan_range_bounded(
        &self,
        table: &str,
        start: &[u8],
        end: Option<&[u8]>,
        max: Option<u32>,
        budget: &mut ReadBudget,
    ) -> Result<Vec<Row>> {
        self.readable()?;
        self.state.check_read(max)?;
        if budget.rows == 0 || end.is_some_and(|end| Key(end.to_vec()) <= Key(start.to_vec())) {
            return Ok(Vec::new());
        }
        scan::Work::run(budget, |work| {
            let mut sources = self.state.range_sources(
                table,
                start,
                end,
                max.unwrap_or(u32::MAX),
                work.clone(),
            )?;
            if max.is_none_or(|max| max >= self.state.version) {
                sources.push(scan::pending_source(
                    &self.pending,
                    self.state.version,
                    table,
                    start,
                    end,
                    work.clone(),
                ));
            }
            scan::merge(sources, &work, |value| match value {
                Payload::Reference(reference) => self.context.value(&self.state.files, reference),
                Payload::Inline(value) => Ok(value.to_vec()),
            })
        })
    }
}
impl Snapshot {
    pub fn scan_range_bounded(
        &self,
        table: &str,
        start: &[u8],
        end: Option<&[u8]>,
        max: Option<u32>,
        budget: &mut ReadBudget,
    ) -> Result<Vec<Row>> {
        self.base.check_read(max)?;
        if budget.rows == 0 || end.is_some_and(|end| Key(end.to_vec()) <= Key(start.to_vec())) {
            return Ok(Vec::new());
        }
        scan::Work::run(budget, |work| {
            let mut sources = if self.cutoff == 0 {
                Vec::new()
            } else {
                self.base.range_sources(
                    table,
                    start,
                    end,
                    max.unwrap_or(u32::MAX).min(self.cutoff - 1),
                    work.clone(),
                )?
            };
            sources.extend(scan::memory_sources(
                &self.local,
                table,
                start,
                end,
                max,
                work.clone(),
            ));
            scan::merge(sources, &work, |value| match value {
                Payload::Reference(reference) => self.context.value(&self.base.files, reference),
                Payload::Inline(value) => Ok(value.to_vec()),
            })
        })
    }
}

impl State {
    fn get_bounded(
        &self,
        context: &Context,
        table: &str,
        key: &[u8],
        max: u32,
        work: &scan::Work,
    ) -> Result<Option<Vec<u8>>> {
        let mut latest = None;
        if let Some(((t, k, Reverse(version)), value)) = self
            .mem
            .range((table.into(), Key(key.to_vec()), Reverse(max))..)
            .next()
        {
            if t == table && k.0 == key {
                work.charge()?;
                latest = Some((*version, *value));
            }
        }
        for view in self.runs.iter().rev() {
            if view.cutoff == 0 {
                continue;
            }
            let limit = max
                .min(view.cutoff - 1)
                .min(view.file.run.metadata().max_version);
            if latest
                .as_ref()
                .is_some_and(|(version, _)| *version >= limit)
            {
                continue;
            }
            if let Some(entry) = view.file.run.get_counted(table, key, limit, Some(work))? {
                if latest
                    .as_ref()
                    .is_some_and(|(version, _)| *version >= entry.version)
                {
                    continue;
                }
                let value = entry
                    .write
                    .value
                    .map(|v| ValueRef::decode(&v))
                    .transpose()?;
                latest = Some((entry.version, value));
            }
        }
        latest
            .and_then(|(_, value)| value)
            .map(|value| {
                work.charge_bytes(
                    key.len()
                        .checked_add(value.size as usize + 8)
                        .ok_or(Error::ReadBudget)?,
                )?;
                context.value(&self.files, value)
            })
            .transpose()
    }
}
impl Native {
    pub fn get_bounded(
        &self,
        table: &str,
        key: &[u8],
        max: Option<u32>,
        budget: &mut ReadBudget,
    ) -> Result<Option<Vec<u8>>> {
        self.readable()?;
        self.state.check_read(max)?;
        if budget.rows == 0 {
            return Ok(None);
        }
        scan::Work::run(budget, |work| {
            work.charge()?;
            if max.is_none_or(|v| v >= self.state.version) {
                if let Some(value) = self.pending.get(&(table.into(), Key(key.to_vec()))) {
                    return value
                        .as_ref()
                        .map(|value| {
                            work.charge_bytes(
                                key.len()
                                    .checked_add(value.len())
                                    .ok_or(Error::ReadBudget)?,
                            )?;
                            Ok(value.clone())
                        })
                        .transpose();
                }
            }
            self.state
                .get_bounded(&self.context, table, key, max.unwrap_or(u32::MAX), &work)
        })
    }
}
impl Snapshot {
    pub fn get_bounded(
        &self,
        table: &str,
        key: &[u8],
        max: Option<u32>,
        budget: &mut ReadBudget,
    ) -> Result<Option<Vec<u8>>> {
        self.base.check_read(max)?;
        if budget.rows == 0 {
            return Ok(None);
        }
        scan::Work::run(budget, |work| {
            work.charge()?;
            if let Some((_, value)) = self.local.row(table, &Key(key.to_vec()), max) {
                return value
                    .as_ref()
                    .map(|value| {
                        work.charge_bytes(
                            key.len()
                                .checked_add(value.len())
                                .ok_or(Error::ReadBudget)?,
                        )?;
                        Ok(value.clone())
                    })
                    .transpose();
            }
            if self.cutoff == 0 {
                return Ok(None);
            }
            self.base.get_bounded(
                &self.context,
                table,
                key,
                max.unwrap_or(u32::MAX).min(self.cutoff - 1),
                &work,
            )
        })
    }
}
