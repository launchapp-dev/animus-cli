use crate::types::{ListPage, ListPageRequest};

pub(super) fn paginate_items<T>(items: Vec<T>, page: ListPageRequest) -> ListPage<T> {
    let total = items.len();
    let (start, end) = page.bounds(total);
    let page_items = items.into_iter().skip(start).take(end.saturating_sub(start)).collect();
    ListPage::new(page_items, total, page)
}
