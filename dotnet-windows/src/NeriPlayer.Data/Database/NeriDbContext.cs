namespace NeriPlayer.Data.Database;

using Microsoft.EntityFrameworkCore;
using NeriPlayer.Data.Entities;

/// <summary>
/// 应用数据库上下文（对标 Room NeriUserDataDatabase / Analysis.md 21.2）。
/// 核心 5 表：songs / playlists / playlist_members / playback_stats / stat_buckets。
/// 后续章节补充：PlayHistory / PlaybackQueue / QueueState / Downloads /
/// DownloadSnapshots / SyncMetadata / SyncOutbox / SyncCheckpoints /
/// TrafficStats / CoverUrlMapping / Settings / CookieCredentials。
/// </summary>
public sealed class NeriDbContext(DbContextOptions<NeriDbContext> options) : DbContext(options)
{
    public DbSet<SongEntity> Songs => Set<SongEntity>();
    public DbSet<PlaylistEntity> Playlists => Set<PlaylistEntity>();
    public DbSet<PlaylistMemberEntity> PlaylistMembers => Set<PlaylistMemberEntity>();
    public DbSet<PlaybackStatsEntity> PlaybackStats => Set<PlaybackStatsEntity>();
    public DbSet<StatBucketEntity> StatBuckets => Set<StatBucketEntity>();

    protected override void OnModelCreating(ModelBuilder b)
    {
        b.Entity<SongEntity>(e =>
        {
            e.ToTable("songs");
            e.HasKey(x => x.Id);
            e.HasIndex(x => x.StableKey).IsUnique();
        });

        b.Entity<PlaylistEntity>(e =>
        {
            e.ToTable("playlists");
            e.HasKey(x => x.Id);
            e.HasMany(p => p.Members)
             .WithOne(m => m.Playlist!)
             .HasForeignKey(m => m.PlaylistId)
             .OnDelete(DeleteBehavior.Cascade);
        });

        b.Entity<PlaylistMemberEntity>(e =>
        {
            e.ToTable("playlist_members");
            e.HasKey(x => new { x.PlaylistId, x.Position });
            e.HasOne(m => m.Song)
             .WithMany()
             .HasForeignKey(m => m.SongId)
             .OnDelete(DeleteBehavior.Cascade);
        });

        b.Entity<PlaybackStatsEntity>(e =>
        {
            e.ToTable("playback_stats");
            e.HasKey(x => x.SongId);
        });

        b.Entity<StatBucketEntity>(e =>
        {
            e.ToTable("stat_buckets");
            e.HasKey(x => new { x.SongId, x.DayKey });
        });
    }
}