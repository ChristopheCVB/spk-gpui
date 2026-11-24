use std::sync::Arc;

use tokio::task::JoinSet;

use super::{
    Sidebar,
    chat::ChatSession,
    presence::{Refresh, Resource},
};
use crate::api::{Channel, Failure, SearchHit, SearchPage, SearchScope, SpkClient};

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
    pending_page: Option<SearchPage>,
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

    pub fn reconcile_channels(&self, sidebar: &mut Sidebar) {
        let channels = sidebar.channels.as_deref().unwrap_or_default();
        let visible = |hit: &SearchHit| conversation(channels, hit.target.channel_id()).is_some();
        if sidebar.search.results.iter().any(|hit| !visible(hit)) {
            Arc::make_mut(&mut sidebar.search.results).retain(visible);
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

    pub fn complete(
        &mut self,
        sidebar: &mut Sidebar,
        refresh: &mut Refresh,
        client: &SpkClient,
        result: Result<SearchPage, Failure>,
    ) {
        let page = match result {
            Ok(page) => page,
            Err(_) => {
                sidebar.search.loading = false;
                sidebar.search.failed = true;
                return;
            }
        };
        let channels = sidebar.channels.as_deref().unwrap_or_default();
        let mut unlisted = false;
        let mut includes_webhook = false;
        for hit in &page.results {
            match conversation(channels, hit.target.channel_id()) {
                Some(channel) => includes_webhook |= channel.is_webhook(),
                None => unlisted = true,
            }
        }
        // Search hits carry no channel type, so an unlisted hit needs a fresh access check.
        if unlisted {
            refresh.fetch(client, Resource::Channels);
        }
        self.pending_page = Some(page);
        if !(unlisted || includes_webhook && refresh.channels_loading()) {
            self.channels_refreshed(sidebar, true);
        }
    }

    pub fn channels_refreshed(&mut self, sidebar: &mut Sidebar, loaded: bool) {
        let Some(mut page) = self.pending_page.take() else {
            return;
        };
        let view = &mut sidebar.search;
        view.loading = false;
        let channels = sidebar.channels.as_deref().unwrap_or_default();
        page.results.retain(|hit| {
            conversation(channels, hit.target.channel_id())
                .is_some_and(|channel| loaded || !channel.is_webhook())
        });
        if !loaded && page.results.is_empty() {
            view.failed = true;
            return;
        }
        view.searched = true;
        Arc::make_mut(&mut view.results).extend(page.results);
        view.next_cursor = page.next_cursor;
    }
}

fn conversation<'a>(channels: &'a [Channel], id: &str) -> Option<&'a Channel> {
    channels
        .iter()
        .find(|channel| channel.id == id && channel.is_conversation())
}

pub(super) fn open_chat_target(
    sidebar: &mut Sidebar,
    chat: &mut ChatSession,
    client: &SpkClient,
    channel_id: &str,
    message_id: Option<&str>,
) -> bool {
    let channels = sidebar.channels.as_deref().unwrap_or_default();
    let Some(channel) = conversation(channels, channel_id) else {
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
