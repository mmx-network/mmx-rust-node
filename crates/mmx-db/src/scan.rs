//! Streaming visible-key merge. Keeps one index entry per input, resolves only
//! the winning value, and does not advance past a satisfied row limit.
use crate::{journal::ValueRef, Database, Error, Key, ReadBudget, Result, Row};
use std::{cell::Cell, cmp::Reverse, collections::BTreeMap, ops::Bound, rc::Rc};

#[derive(Clone)]
pub(crate) struct Work(Rc<Cell<ReadBudget>>);
impl Work {
    pub fn run<T>(budget: &mut ReadBudget, call: impl FnOnce(Work) -> Result<T>) -> Result<T> {
        let work = Self(Rc::new(Cell::new(*budget)));
        let result = call(work.clone());
        *budget = work.0.get();
        result
    }
    pub fn rows(&self) -> usize {
        self.0.get().rows
    }
    pub fn charge(&self) -> Result<()> {
        let mut left = self.0.get();
        left.keys = left.keys.checked_sub(1).ok_or(Error::ReadBudget)?;
        self.0.set(left);
        Ok(())
    }
    pub fn charge_bytes(&self, size: usize) -> Result<()> {
        let mut left = self.0.get();
        left.bytes = left.bytes.checked_sub(size).ok_or(Error::ReadBudget)?;
        self.0.set(left);
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Payload<'a> {
    Reference(ValueRef),
    Inline(&'a [u8]),
}
impl Payload<'_> {
    fn len(self) -> usize {
        match self {
            // Include a possible lazy segment-header read on a cache miss.
            Self::Reference(v) => v.size as usize + 8,
            Self::Inline(v) => v.len(),
        }
    }
}
pub(crate) struct Item<'a> {
    pub key: Key,
    pub version: u32,
    pub value: Option<Payload<'a>>,
}
pub(crate) type Source<'a> = Box<dyn Iterator<Item = Result<Item<'a>>> + 'a>;
type Order = (Key, Reverse<u32>, Reverse<usize>);
pub(crate) fn merge<'a>(
    mut sources: Vec<Source<'a>>,
    work: &Work,
    mut resolve: impl FnMut(Payload<'a>) -> Result<Vec<u8>>,
) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    if work.rows() == 0 {
        return Ok(out);
    }
    let mut heap: BTreeMap<Order, Option<Payload<'a>>> = BTreeMap::new();
    for (index, source) in sources.iter_mut().enumerate() {
        if let Some(item) = source.next().transpose()? {
            heap.insert(
                (item.key, Reverse(item.version), Reverse(index)),
                item.value,
            );
        }
    }
    while let Some(((key, Reverse(version), Reverse(index)), value)) = heap.pop_first() {
        let mut advance = vec![index];
        while heap
            .first_key_value()
            .is_some_and(|((next, _, _), _)| *next == key)
        {
            let ((_, _, Reverse(index)), _) = heap.pop_first().unwrap();
            advance.push(index);
        }
        if let Some(value) = value {
            work.charge_bytes(
                key.0
                    .len()
                    .checked_add(value.len())
                    .ok_or(Error::ReadBudget)?,
            )?;
            out.push(Row {
                key: key.0,
                version,
                value: resolve(value)?,
            });
            if out.len() == work.rows() {
                return Ok(out);
            }
        }
        for index in advance {
            if let Some(item) = sources[index].next().transpose()? {
                heap.insert(
                    (item.key, Reverse(item.version), Reverse(index)),
                    item.value,
                );
            }
        }
    }
    Ok(out)
}

pub(crate) fn memory_sources<'a>(
    db: &'a Database,
    table: &str,
    start: &[u8],
    end: Option<&[u8]>,
    max: Option<u32>,
    work: Work,
) -> Vec<Source<'a>> {
    let mut sources: Vec<Source<'a>> = Vec::new();
    let first = Key(start.to_vec());
    let last = end.map(|key| Key(key.to_vec()));
    if last.as_ref().is_some_and(|end| *end <= first) {
        return sources;
    }
    if let Some(rows) = db.tables.get(table) {
        let rows = rows.range((
            Bound::Included(first.clone()),
            last.clone().map_or(Bound::Unbounded, Bound::Excluded),
        ));
        let work = work.clone();
        sources.push(Box::new(rows.filter_map(move |(key, versions)| {
            if let Err(error) = work.charge() {
                return Some(Err(error));
            }
            if let Err(error) = work.charge_bytes(key.0.len()) {
                return Some(Err(error));
            }
            let (version, value) = versions.range(..=max.unwrap_or(u32::MAX)).next_back()?;
            Some(Ok(Item {
                key: key.clone(),
                version: *version,
                value: value.as_deref().map(Payload::Inline),
            }))
        })));
    }
    if max.is_none_or(|max| max >= db.version) {
        sources.push(pending_source(
            &db.pending,
            db.version,
            table,
            start,
            end,
            work,
        ));
    }
    sources
}
pub(crate) fn pending_source<'a>(
    pending: &'a crate::Pending,
    version: u32,
    table: &str,
    start: &[u8],
    end: Option<&[u8]>,
    work: Work,
) -> Source<'a> {
    let rows = pending.range((table.to_owned(), Key(start.to_vec()))..);
    let table = table.to_owned();
    let end = end.map(|v| Key(v.to_vec()));
    Box::new(
        rows.take_while(move |((t, key), _)| {
            *t == table && end.as_ref().is_none_or(|end| *key < *end)
        })
        .map(move |((_, key), value)| {
            work.charge()?;
            work.charge_bytes(key.0.len())?;
            Ok(Item {
                key: key.clone(),
                version,
                value: value.as_deref().map(Payload::Inline),
            })
        }),
    )
}
