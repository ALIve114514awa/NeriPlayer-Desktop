namespace NeriPlayer.Data.Repositories;

using Microsoft.EntityFrameworkCore;
using NeriPlayer.Data.Database;
using NeriPlayer.Data.Entities;

/// <summary>
/// 歌曲仓储（对标 start.md 4.5 / Process.md 5.1）。
/// Upsert 语义：按 StableKey 查询，存在则更新、不存在则插入。
/// </summary>
public sealed class SongRepository(NeriDbContext db)
{
    /// <summary>按 StableKey 查询单首歌曲</summary>
    public async Task<SongEntity?> GetByStableKeyAsync(string stableKey) =>
        await db.Songs.FirstOrDefaultAsync(s => s.StableKey == stableKey);

    /// <summary>插入或更新歌曲，返回持久化后的 Id（StableKey 唯一约束保护）</summary>
    public async Task<long> UpsertAsync(SongEntity song)
    {
        var existing = await GetByStableKeyAsync(song.StableKey);
        if (existing is not null)
        {
            db.Entry(existing).CurrentValues.SetValues(song);
            await db.SaveChangesAsync();
            return existing.Id;
        }
        db.Songs.Add(song);
        await db.SaveChangesAsync();
        return song.Id;
    }
}