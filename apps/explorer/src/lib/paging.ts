export const PAGE_SIZE = 20;

export function pageSlice<T>(data: T[], requested = 1) {
  const pages = Math.max(1, Math.ceil(data.length / PAGE_SIZE));
  const page = Math.min(Math.max(1, requested), pages);
  return {
    rows: data.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE),
    page,
    pages,
    start: data.length ? (page - 1) * PAGE_SIZE + 1 : 0,
    end: Math.min(page * PAGE_SIZE, data.length),
  };
}
