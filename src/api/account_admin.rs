use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    pub id: String,
    pub note: Option<String>,
    pub expires_at: i64,
    pub status: InviteStatus,
    pub used_by: Option<InviteUser>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InviteStatus {
    Active,
    Used,
    Expired,
    Revoked,
}

#[derive(Deserialize)]
pub struct InviteUser {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDirectory {
    pub users: Vec<AdminUser>,
    pub total_bytes: u64,
}

#[derive(Deserialize)]
pub struct AdminUser {
    #[serde(flatten)]
    pub user: User,
    pub email: String,
    pub bytes: u64,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccountRole {
    Admin,
    User,
}

#[derive(Deserialize)]
pub struct IssuedAccountLink {
    pub id: Option<String>,
    pub url: String,
}

impl SpkClient {
    pub async fn invites(&self) -> Result<Vec<Invite>, Failure> {
        self.get("/api/invites").await
    }

    pub async fn admin_users(&self) -> Result<AdminDirectory, Failure> {
        self.get("/api/admin/users").await
    }

    pub async fn create_invite(&self, note: &str) -> Result<IssuedAccountLink, Failure> {
        let request = self
            .request(Method::POST, "/api/invites")
            .json(&serde_json::json!({ "note": note }));
        let link = self.issued_link(request, "/signup", "invite").await?;
        if link.id.as_deref().is_none_or(str::is_empty) {
            return Err(Failure::InvalidResponse);
        }
        Ok(link)
    }

    pub async fn revoke_invite(&self, id: &str) -> Result<(), Failure> {
        let path = admin_account_path("/api/invites", id, "revoke")?;
        Self::send(self.request(Method::POST, &path)).await?;
        Ok(())
    }

    pub async fn issue_password_reset(&self, id: &str) -> Result<IssuedAccountLink, Failure> {
        let path = admin_account_path("/api/admin/users", id, "reset-link")?;
        self.issued_link(
            self.request(Method::POST, &path),
            "/reset-password",
            "token",
        )
        .await
    }

    pub async fn set_account_role(&self, id: &str, role: AccountRole) -> Result<(), Failure> {
        let path = admin_account_path("/api/admin/users", id, "role")?;
        Self::send(
            self.request(Method::POST, &path)
                .json(&serde_json::json!({ "role": role })),
        )
        .await?;
        Ok(())
    }

    pub async fn set_account_banned(
        &self,
        id: &str,
        banned: bool,
        reason: &str,
    ) -> Result<(), Failure> {
        let path = admin_account_path("/api/admin/users", id, "ban")?;
        Self::send(self.request(Method::POST, &path).json(&serde_json::json!({
            "banned": banned,
            "reason": reason,
        })))
        .await?;
        Ok(())
    }

    async fn issued_link(
        &self,
        request: reqwest::RequestBuilder,
        path: &str,
        token_key: &str,
    ) -> Result<IssuedAccountLink, Failure> {
        let link: IssuedAccountLink = Self::send(request)
            .await?
            .json()
            .await
            .map_err(|_| Failure::InvalidResponse)?;
        let url = Url::parse(&link.url).map_err(|_| Failure::InvalidResponse)?;
        if url.origin() != self.base.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.path() != path
            || !url
                .query_pairs()
                .any(|(key, token)| key == token_key && !token.is_empty())
        {
            return Err(Failure::InvalidResponse);
        }
        Ok(link)
    }
}

fn admin_account_path(prefix: &str, id: &str, action: &str) -> Result<String, Failure> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(Failure::InvalidResponse);
    }
    Ok(format!("{prefix}/{id}/{action}"))
}
