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

        // 后台（第九章实现后取消注释）
        // services.AddHostedService<Background.Services.SyncScheduledService>();

        return services.BuildServiceProvider();
    }
}
