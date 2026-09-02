use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feed {
    pub id: i32,
    pub url: String,
    pub title: Option<String>,
    pub site_url: Option<String>,
    pub favicon_url: Option<String>,
    pub last_fetched_at: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Article {
    pub id: i32,
    pub feed_id: i32,
    pub title: String,
    pub link: String,
    pub guid: String,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub published_at: Option<String>,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub id: i32,
    pub user_id: i32,
    pub feed_id: i32,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateFeedRequest {
    pub url: String,
    pub title: Option<String>,
    pub site_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SubscribeFeedRequest {
    pub user_id: i32,
    pub feed_id: i32,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_feed() -> Feed {
        Feed {
            id: 1,
            url: "https://example.com/rss.xml".to_string(),
            title: Some("Example".to_string()),
            site_url: Some("https://example.com".to_string()),
            favicon_url: None,
            last_fetched_at: Some("2026-09-01T10:00:00Z".to_string()),
            status: "active".to_string(),
        }
    }

    fn sample_article() -> Article {
        Article {
            id: 9,
            feed_id: 1,
            title: "Hello".to_string(),
            link: "https://example.com/hello".to_string(),
            guid: "guid-9".to_string(),
            summary: Some("sum".to_string()),
            content: None,
            published_at: Some("2026-08-01T10:00:00Z".to_string()),
            hash: "abcd1234abcd1234abcd1234abcd1234".to_string(),
        }
    }

    #[test]
    fn feed_serializes_and_deserializes() {
        let feed = sample_feed();
        let json = serde_json::to_string(&feed).expect("serialize");
        let back: Feed = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(feed, back);

        // Null optionals must come back as null, not error.
        let partial: Feed =
            serde_json::from_str(r#"{"id":2,"url":"https://a/b","title":null,"site_url":null,"favicon_url":null,"last_fetched_at":null,"status":"pending"}"#)
                .expect("deserialize partial");
        assert_eq!(partial.title, None);
        assert_eq!(partial.site_url, None);
        assert_eq!(partial.status, "pending");
    }

    #[test]
    fn article_serializes_and_deserializes() {
        let article = sample_article();
        let json = serde_json::to_string(&article).expect("serialize");
        let back: Article = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(article, back);
        assert_eq!(back.hash.len(), 32);
    }

    #[test]
    fn subscription_serializes_and_deserializes() {
        let sub = Subscription {
            id: 3,
            user_id: 7,
            feed_id: 1,
            created_at: "2026-09-02T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&sub).expect("serialize");
        let back: Subscription = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(sub, back);
    }

    #[test]
    fn api_response_roundtrips_with_data() {
        let response: ApiResponse<Vec<Article>> = ApiResponse {
            success: true,
            data: Some(vec![sample_article()]),
            error: None,
        };
        let json = serde_json::to_string(&response).expect("serialize");
        let back: ApiResponse<Vec<Article>> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(response, back);
        assert!(json.contains("\"success\":true"));
    }

    #[test]
    fn api_response_roundtrips_error_without_data() {
        let response: ApiResponse<()> = ApiResponse {
            success: false,
            data: None,
            error: Some("Not implemented".to_string()),
        };
        let json = serde_json::to_string(&response).expect("serialize");
        let back: ApiResponse<()> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(response, back);

        // A success response with no data serializes data as null.
        let empty: ApiResponse<()> = serde_json::from_str(r#"{"success":true,"data":null,"error":null}"#)
            .expect("deserialize empty");
        assert!(empty.success);
        assert_eq!(empty.data, None);
        assert_eq!(empty.error, None);
    }

    #[test]
    fn create_feed_request_deserializes_with_optional_fields() {
        // Both JSON shapes accepted by handlers must deserialize.
        let full: CreateFeedRequest =
            serde_json::from_str(r#"{"url":"https://example.com/rss","title":"T","site_url":"https://example.com"}"#)
                .expect("full request");
        assert_eq!(full.url, "https://example.com/rss");
        assert_eq!(full.title.as_deref(), Some("T"));
        assert_eq!(full.site_url.as_deref(), Some("https://example.com"));

        let minimal: CreateFeedRequest =
            serde_json::from_str(r#"{"url":"https://example.com/rss"}"#).expect("minimal request");
        assert_eq!(minimal.title, None);
        assert_eq!(minimal.site_url, None);
    }

    #[test]
    fn subscribe_feed_request_deserializes() {
        let req: SubscribeFeedRequest =
            serde_json::from_str(r#"{"user_id":42,"feed_id":13}"#).expect("subscribe request");
        assert_eq!(req.user_id, 42);
        assert_eq!(req.feed_id, 13);

        assert!(serde_json::from_str::<SubscribeFeedRequest>(r#"{"user_id":42}"#).is_err());
    }

    #[test]
    fn api_response_rejects_missing_success_field() {
        let result: serde_json::Result<ApiResponse<()>> =
            serde_json::from_str(r#"{"data":null,"error":null}"#);
        assert!(result.is_err());
    }
}
