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
