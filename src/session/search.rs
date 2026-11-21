use std::sync::Arc;

use tokio::task::JoinSet;

use super::{Sidebar, chat::ChatSession};
use crate::api::{Failure, MessageTarget, SearchHit, SearchPage, SearchScope, SpkClient};

#[derive(Clone, Default)]
pub struct SearchView {
    pub results: Arc<Vec<SearchHit>>,
    pub next_cursor: Option<String>,
    pub loading: bool,
    pub searched: bool,
    pub failed: bool,
}

#[derive(Default)]
pub(super) struct SearchSession {
    query: Option<(String, SearchScope)>,
    pub tasks: JoinSet<Result<SearchPage, Failure>>,
}

impl SearchSession {
    pub fn start(
        &mut self,
        view: &mut SearchView,
        client: &SpkClient,
        query: String,
        scope: SearchScope,
    ) {
        *self = Self::default();
        *view = SearchView::default();
        let query = query.trim().to_owned();
        if !query.is_empty() {
            self.query = Some((query, scope));
            self.load(view, client);
        }
    }

    pub fn more(&mut self, view: &mut SearchView, client: &SpkClient) {
        if !view.loading && view.next_cursor.is_some() {
            self.load(view, client);
        }
    }

    fn load(&mut self, view: &mut SearchView, client: &SpkClient) {
        let Some((query, scope)) = self.query.clone() else {
            return;
        };
        view.loading = true;
        view.failed = false;
        let cursor = view.next_cursor.clone();
        let client = client.clone();
        self.tasks.spawn(async move {
            client
                .search_messages(&query, &scope, cursor.as_deref())
                .await
        });
    }

    pub fn complete(view: &mut SearchView, result: Result<SearchPage, Failure>) {
        view.loading = false;
        match result {
            Ok(page) => {
                view.searched = true;
                Arc::make_mut(&mut view.results).extend(page.results);
                view.next_cursor = page.next_cursor;
            }
            Err(_) => view.failed = true,
        }
    }
}

pub(super) fn open_message_target(
    sidebar: &mut Sidebar,
    chat: &mut ChatSession,
    client: &SpkClient,
    target: &MessageTarget,
) -> bool {
    let Some(channel) = sidebar.channels.iter().flatten().find(|channel| {
        channel.id == target.channel_id()
            && (channel.kind == "text" || channel.kind == "dm" && channel.other_user_id.is_some())
    }) else {
        return false;
    };
    chat.open_target(client, channel, target.message_id().to_owned());
    sidebar.chat_notice = None;
    true
}
