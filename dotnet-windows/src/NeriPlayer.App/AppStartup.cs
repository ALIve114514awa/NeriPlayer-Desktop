using Microsoft.Extensions.DependencyInjection;

namespace NeriPlayer.App;

public static class AppStartup
{
    public static ServiceProvider BuildServices()
    {
        var services = new ServiceCollection();

        // 数据层（第四章实现后取消注释）
        // services.AddDbContext<Data.Database.NeriDbContext>();

        // 核心层（第五/七章实现后取消注释）
        // services.AddSingleton<Core.Player.PlayerManager>();
        // services.AddSingleton<Core.Api.Common.HttpClientFactory>();
        // services.AddSingleton<Core.Api.Netease.NeteaseClient>();
        // services.AddSingleton<Core.Api.Bili.BiliClient>();
        // services.AddSingleton<Core.Api.YouTube.YouTubeMusicClient>();

        // 后台（第九章实现后取消注释）
        // services.AddHostedService<Background.Services.SyncScheduledService>();

        return services.BuildServiceProvider();
    }
}
