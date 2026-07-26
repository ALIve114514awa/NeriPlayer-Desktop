// 平台请求的传输层兜底
//
// 单一的「绕过代理」开关覆盖不了现实：系统代理配错时只有直连能通，
// 处在需要代理才能出网的网络里又只有走代理能通，而用户不一定知道该拨哪边。
// 这里统一提供「主客户端失败后用相反代理设置重试一次」的能力，
// 各平台客户端共用，避免每个平台各写一份。

use reqwest::{Client, RequestBuilder, Response};

/// 是否属于「换个代理设置可能就通」的失败
///
/// 只认连接建立阶段的错误。HTTP 状态码错误在 reqwest 里是 `Ok(response)`，
/// 本来就到不了这里；解析错误重试也没有意义，只会把一次业务失败放大成两次请求。
pub fn is_transport_failure(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout() || (error.is_request() && !error.is_decode())
}

/// 带代理兜底的 HTTP 通道
#[derive(Clone)]
pub struct FallbackHttp {
    primary: Client,
    /// 代理设置与 primary 相反；None 表示不做兜底
    fallback: Option<Client>,
    /// 日志标签，用于区分是哪个平台在兜底
    target: &'static str,
}

impl FallbackHttp {
    pub fn new(primary: &Client, target: &'static str) -> Self {
        Self {
            primary: primary.clone(),
            fallback: None,
            target,
        }
    }

    pub fn with_fallback(primary: &Client, fallback: &Client, target: &'static str) -> Self {
        Self {
            primary: primary.clone(),
            fallback: Some(fallback.clone()),
            target,
        }
    }

    /// 主客户端，供不需要兜底或需要自行构造请求的调用方使用
    pub fn primary(&self) -> &Client {
        &self.primary
    }

    /// 发送请求；传输层失败时用相反代理设置重试一次
    ///
    /// `build` 会被调用最多两次，因此必须是可重复执行的纯构造，
    /// 不要在里面做带副作用的操作。
    pub async fn send(
        &self,
        build: impl Fn(&Client) -> RequestBuilder,
    ) -> Result<Response, reqwest::Error> {
        let error = match build(&self.primary).send().await {
            Ok(response) => return Ok(response),
            Err(error) => error,
        };
        let Some(fallback) = self.fallback.as_ref().filter(|_| is_transport_failure(&error)) else {
            return Err(error);
        };
        log::warn!(
            target: "transport",
            "{} primary transport failed, retrying with the opposite proxy setting: {}",
            self.target,
            error,
        );
        build(fallback).send().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// 测试必须绕开系统代理：mock server 监听回环地址
    fn client() -> Client {
        Client::builder()
            .no_proxy()
            .build()
            .expect("failed to build test client")
    }

    /// 起一个只回固定状态码的服务，并统计收到的请求数
    async fn mock_server(status: &'static str) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { break };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0_u8; 1024];
                let _ = socket.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://{address}"), hits)
    }

    /// 关掉监听后端口立刻拒绝连接，用来模拟「代理配错，主路不通」
    async fn dead_address() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}")
    }

    #[tokio::test]
    async fn primary_success_never_touches_the_fallback() {
        let (primary_url, primary_hits) = mock_server("200 OK").await;
        let (_fallback_url, fallback_hits) = mock_server("200 OK").await;
        let transport = FallbackHttp::with_fallback(&client(), &client(), "test");

        let response = transport
            .send(|client| client.get(&primary_url))
            .await
            .unwrap();

        assert!(response.status().is_success());
        assert_eq!(primary_hits.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn transport_failure_retries_once_on_the_fallback() {
        let dead = dead_address().await;
        let (live_url, live_hits) = mock_server("200 OK").await;
        // 用 URL 区分两条路：主路指向已关闭端口，兜底指向存活服务
        let transport = FallbackHttp::with_fallback(&client(), &client(), "test");
        let attempt = AtomicUsize::new(0);

        let response = transport
            .send(|client| {
                let url = if attempt.fetch_add(1, Ordering::SeqCst) == 0 {
                    dead.clone()
                } else {
                    live_url.clone()
                };
                client.get(url)
            })
            .await
            .unwrap();

        assert!(response.status().is_success());
        assert_eq!(attempt.load(Ordering::SeqCst), 2, "should have retried once");
        assert_eq!(live_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn http_error_status_is_not_retried() {
        // 5xx 在 reqwest 里是 Ok(response)，重试只会把一次业务失败放大成两次请求
        let (url, hits) = mock_server("500 Internal Server Error").await;
        let transport = FallbackHttp::with_fallback(&client(), &client(), "test");

        let response = transport.send(|client| client.get(&url)).await.unwrap();

        assert_eq!(response.status().as_u16(), 500);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "must not double the request");
    }

    #[tokio::test]
    async fn without_a_fallback_the_original_error_is_returned() {
        let dead = dead_address().await;
        let transport = FallbackHttp::new(&client(), "test");

        let error = transport.send(|client| client.get(&dead)).await.unwrap_err();

        assert!(is_transport_failure(&error));
    }
}
