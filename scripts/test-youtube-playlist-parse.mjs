import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import ts from 'typescript'

async function compileTs(relativePath) {
  const sourceUrl = new URL(relativePath, import.meta.url)
  const source = await readFile(sourceUrl, 'utf8')
  return ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.ES2022,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText
}

const trackCoverCompiled = await compileTs('../src/utils/trackCover.ts')
const trackCoverModuleUrl = `data:text/javascript;base64,${Buffer.from(trackCoverCompiled).toString('base64')}`

const playlistSourceUrl = new URL('../src/modules/youtube/youtubePlaylistParse.ts', import.meta.url)
const playlistSource = (await readFile(playlistSourceUrl, 'utf8'))
  .replace("from '@/utils/trackCover'", `from ${JSON.stringify(trackCoverModuleUrl)}`)
  .replace(/import type \{ TrackInfo \} from '@\/stores\/player'\n?/, '')
const playlistCompiled = ts.transpileModule(playlistSource, {
  compilerOptions: {
    module: ts.ModuleKind.ES2022,
    target: ts.ScriptTarget.ES2022,
  },
}).outputText
const moduleUrl = `data:text/javascript;base64,${Buffer.from(playlistCompiled).toString('base64')}`
const {
  extractTrackVideoId,
  parseYouTubeLibraryPlaylists,
  parseYouTubePlaylistTracks,
  parseYouTubePlaylistMeta,
} = await import(moduleUrl)

// videoId 多路径提取
assert.equal(
  extractTrackVideoId({
    playlistItemData: { videoId: 'abc123' },
  }),
  'abc123',
)
assert.equal(
  extractTrackVideoId({
    overlay: {
      musicItemThumbnailOverlayRenderer: {
        content: {
          musicPlayButtonRenderer: {
            playNavigationEndpoint: {
              watchEndpoint: { videoId: 'fromOverlay' },
            },
          },
        },
      },
    },
  }),
  'fromOverlay',
)

// 库内歌单：grid + twoRow
const libraryRoot = {
  contents: {
    singleColumnBrowseResultsRenderer: {
      tabs: [{
        tabRenderer: {
          content: {
            sectionListRenderer: {
              contents: [{
                gridRenderer: {
                  items: [{
                    musicTwoRowItemRenderer: {
                      title: { runs: [{ text: 'My Playlist' }] },
                      subtitle: { runs: [{ text: '12 songs' }] },
                      navigationEndpoint: {
                        browseEndpoint: {
                          browseId: 'VLplaylist1',
                          browseEndpointContextSupportedConfigs: {
                            browseEndpointContextMusicConfig: {
                              pageType: 'MUSIC_PAGE_TYPE_PLAYLIST',
                            },
                          },
                        },
                      },
                      thumbnailRenderer: {
                        musicThumbnailRenderer: {
                          thumbnail: {
                            thumbnails: [{ url: 'https://img/cover.jpg' }],
                          },
                        },
                      },
                    },
                  }],
                },
              }],
            },
          },
        },
      }],
    },
  },
}

const playlists = parseYouTubeLibraryPlaylists(libraryRoot)
assert.equal(playlists.length, 1)
assert.equal(playlists[0].id, 'VLplaylist1')
assert.equal(playlists[0].name, 'My Playlist')
assert.equal(playlists[0].trackCount, 12)
assert.equal(playlists[0].coverUrl, 'https://img/cover.jpg')

// 歌单详情：secondaryContents + playlistItemData videoId
const detailRoot = {
  header: {
    musicDetailHeaderRenderer: {
      title: { runs: [{ text: 'Detail PL' }] },
      subtitle: { runs: [{ text: 'Album - 2024' }] },
      thumbnail: {
        croppedSquareThumbnailRenderer: {
          thumbnail: { thumbnails: [{ url: 'https://img/pl.jpg' }] },
        },
      },
    },
  },
  contents: {
    twoColumnBrowseResultsRenderer: {
      secondaryContents: {
        sectionListRenderer: {
          contents: [{
            musicPlaylistShelfRenderer: {
              contents: [{
                musicResponsiveListItemRenderer: {
                  playlistItemData: { videoId: 'vid001' },
                  flexColumns: [
                    {
                      musicResponsiveListItemFlexColumnRenderer: {
                        text: { runs: [{ text: 'Song A' }] },
                      },
                    },
                    {
                      musicResponsiveListItemFlexColumnRenderer: {
                        text: { runs: [{ text: 'Artist A' }] },
                      },
                    },
                  ],
                  fixedColumns: [{
                    musicResponsiveListItemFixedColumnRenderer: {
                      text: { runs: [{ text: '3:21' }] },
                    },
                  }],
                  thumbnail: {
                    musicThumbnailRenderer: {
                      thumbnail: { thumbnails: [{ url: 'https://img/track.jpg' }] },
                    },
                  },
                },
              }],
            },
          }],
        },
      },
    },
  },
}

const meta = parseYouTubePlaylistMeta(detailRoot)
assert.equal(meta.playlistName, 'Detail PL')
assert.equal(meta.coverUrl, 'https://img/pl.jpg')

const tracks = parseYouTubePlaylistTracks(detailRoot)
assert.equal(tracks.length, 1)
assert.equal(tracks[0].id, 'youtube:vid001')
assert.equal(tracks[0].title, 'Song A')
assert.equal(tracks[0].artist, 'Artist A')
assert.equal(tracks[0].durationMs, (3 * 60 + 21) * 1000)
assert.equal(tracks[0].syncPayload?.audioId, 'vid001')

// 新版 YTM: header 在 twoColumn tabs section 的 musicResponsiveHeaderRenderer 中
const responsiveHeaderRoot = {
  contents: {
    twoColumnBrowseResultsRenderer: {
      tabs: [{
        tabRenderer: {
          content: {
            sectionListRenderer: {
              contents: [{
                musicResponsiveHeaderRenderer: {
                  title: { runs: [{ text: 'Responsive PL' }] },
                  subtitle: { runs: [{ text: '12 songs' }] },
                  thumbnail: {
                    musicThumbnailRenderer: {
                      thumbnail: { thumbnails: [{ url: 'https://img/responsive-cover.jpg' }] },
                    },
                  },
                },
              }],
            },
          },
        },
      }],
      secondaryContents: {
        sectionListRenderer: {
          contents: [{
            musicPlaylistShelfRenderer: {
              contents: [{
                musicResponsiveListItemRenderer: {
                  playlistItemData: { videoId: 'vid-r1' },
                  flexColumns: [
                    { musicResponsiveListItemFlexColumnRenderer: { text: { runs: [{ text: 'R1' }] } } },
                    { musicResponsiveListItemFlexColumnRenderer: { text: { runs: [{ text: 'A1' }] } } },
                  ],
                  fixedColumns: [{
                    musicResponsiveListItemFixedColumnRenderer: {
                      text: { runs: [{ text: '1:00' }] },
                    },
                  }],
                  thumbnail: {
                    musicThumbnailRenderer: {
                      thumbnail: { thumbnails: [{ url: 'https://img/t1.jpg' }] },
                    },
                  },
                },
              }],
            },
          }],
        },
      },
    },
  },
}

const responsiveMeta = parseYouTubePlaylistMeta(responsiveHeaderRoot)
assert.equal(responsiveMeta.playlistName, 'Responsive PL')
assert.equal(responsiveMeta.coverUrl, 'https://img/responsive-cover.jpg')
assert.equal(parseYouTubePlaylistTracks(responsiveHeaderRoot).length, 1)

const viewSource = await readFile(
  new URL('../src/views/YouTubePlaylistView.vue', import.meta.url),
  'utf8',
)
assert.match(viewSource, /parseYouTubePlaylistTracks/)
assert.match(viewSource, /parseYouTubePlaylistMeta/)

const recommendSource = await readFile(
  new URL('../src/stores/recommend.ts', import.meta.url),
  'utf8',
)
assert.match(recommendSource, /parseYouTubeLibraryPlaylistsShared/)

console.log('youtube playlist parse tests passed')
