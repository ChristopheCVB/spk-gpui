use std::sync::Arc;

use tokio::task::JoinSet;

use super::{Sidebar, chat::ChatSession};
use crate::api::{Failure, SearchHit, SearchPage, SearchScope, SpkClient};

#[derive(Clone, Default)]
pub struct SearchView {
    pub scope: Option<SearchScope>,
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
            view.scope = Some(scope.clone());
            self.query = Some((query, scope));
            self.load(view, client);
        }
    }

    pub fn retain_conversation(&mut self, view: &mut SearchView, channel: Option<&str>) {
        if let Some(SearchScope::Conversation(id)) = &view.scope
            && channel != Some(id.as_str())
        {
            *self = Self::default();
            *view = SearchView::default();
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

pub(super) fn open_chat_target(
    sidebar: &mut Sidebar,
    chat: &mut ChatSession,
    client: &SpkClient,
    channel_id: &str,
    message_id: Option<&str>,
) -> bool {
    let Some(channel) = sidebar
        .channels
        .iter()
        .flatten()
        .find(|channel| channel.id == channel_id && channel.is_conversation())
    else {
        return false;
    };
    if let Some(message_id) = message_id {
        chat.open_target(client, channel, message_id.to_owned());
    } else {
        chat.open_conversation_target(client, channel);
    }
    sidebar.chat_notice = None;
    true
}
