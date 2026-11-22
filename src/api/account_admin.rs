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
        account_link_token(&self.base, &url, path, token_key).ok_or(Failure::InvalidResponse)?;
        Ok(link)
    }
}

pub(crate) fn account_link_token(base: &Url, url: &Url, path: &str, key: &str) -> Option<String> {
    if url.origin() != base.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path() != path
    {
        return None;
    }
    let mut tokens = url
        .query_pairs()
        .filter_map(|(name, value)| (name == key).then_some(value));
    let token = tokens.next()?;
    if tokens.next().is_some()
        || token.is_empty()
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return None;
    }
    Some(token.into_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_links_require_a_single_valid_token_for_the_expected_instance_and_form() {
        let base = Url::parse("https://spk.example").unwrap();
        let parse = |url| account_link_token(&base, &Url::parse(url).unwrap(), "/signup", "invite");
        assert_eq!(
            parse("https://spk.example/signup?invite=abc_123-"),
            Some("abc_123-".into())
        );
        for url in [
            "https://spk.example/signup?invite=abc&%69nvite=def",
            "https://spk.example/signup?invite=abc&invite=",
            "https://spk.example/signup?invite=abc%2Fdef",
            "https://spk.example/signup?invite=abc+def",
            "https://spk.example/signup?invite=",
            "https://spk.example/signup?token=abc",
            "https://spk.example/signup?invite=abc#fragment",
            "https://user@spk.example/signup?invite=abc",
            "https://other.example/signup?invite=abc",
            "https://spk.example/reset-password?invite=abc",
        ] {
            assert_eq!(parse(url), None, "{url}");
        }
    }
}
