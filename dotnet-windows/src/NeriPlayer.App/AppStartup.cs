using Microsoft.Extensions.DependencyInjection;
using NeriPlayer.Core.Api.Common;

namespace NeriPlayer.App;

public static class AppStartup
{
    public static ServiceProvider BuildServices()
    {
        var services = new ServiceCollection();

        // 数据层
        services.AddDbContext<Data.Database.NeriDbContext>();

        // 核心层
        services.AddSingleton<Core.Player.PlayerManager>();

        // 下载管理（第八章）
        services.AddSingleton<Core.Download.DownloadQueue>(sp =>
        {
            var factory = sp.GetRequiredService<HttpClientFactory>();
            return new Core.Download.DownloadQueue(factory.Http);
        });
        services.AddScoped<Data.Repositories.DownloadRepository>();

        // API 客户端（第七章）
        services.AddSingleton<HttpClientFactory>();
        services.AddSingleton<Core.Api.Netease.NeteaseClient>();
        services.AddSingleton<Core.Api.Bili.BiliClient>();
        services.AddSingleton<Core.Api.YouTube.YouTubePlayerScriptStore>();
        services.AddSingleton<Core.Api.YouTube.YouTubeMusicClient>(sp =>
            new Core.Api.YouTube.YouTubeMusicClient(
                sp.GetRequiredService<HttpClientFactory>(),
                sp.GetRequiredService<Core.Api.YouTube.YouTubePlayerScriptStore>()));

        // 第九章：数据同步
        services.AddScoped<Data.Repositories.SyncRepository>();
        services.AddScoped<Data.Repositories.SongRepository>();
        services.AddSingleton<Data.Auth.CredentialStore>();
        services.AddSingleton<Data.Sync.GitHubSyncProvider>(sp =>
            new Data.Sync.GitHubSyncProvider(
                token: "your-github-token",     // 实际运行时从 CredentialStore / 配置读取
                owner: "your-github-owner",
                repo: "neriplayer-sync"));
        services.AddSingleton<Data.Sync.WebDavSyncProvider>(sp =>
            new Data.Sync.WebDavSyncProvider(
                server: new Uri("https://dav.example.com"),
                user: "user",
                password: "password"));
        services.AddScoped<Data.Sync.SyncCoordinator>(sp =>
            new Data.Sync.SyncCoordinator(
                sp.GetRequiredService<Data.Sync.GitHubSyncProvider>(),
                sp.GetRequiredService<Data.Repositories.SongRepository>(),
                sp.GetRequiredService<Data.Repositories.SyncRepository>()));

        // 后台（第十章 UI 完成后取消注释，供定时同步）
        // services.AddHostedService<Background.Services.SyncScheduledService>();

        return services.BuildServiceProvider();
    }
}
