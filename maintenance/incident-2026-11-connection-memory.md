# Incident record: memory that never came back (recorded 9 November 2026)

The recorded incident behind `axum-service`'s serve rule (`## Important`, Red Flags, and
`references/operations.md` "Memory after load") and `rust-testing`'s
`references/connection-memory.md`. Three axum 0.8 services, a platform API and two game servers,
behind Envoy. The numbers under "Reported" come from the incident report and were not reproduced
here; those under "Reproduced" were measured in this repository.

## Reported

- Bare `axum::serve(listener, router)`. A docs route, `get(move || async move { Json(openapi) })`,
  owned the OpenAPI document: about 4 MB per connection.
- Pods grew from 137 MB to 373 MB under load and stayed there; the memory HPA stayed at max replicas.
- 50 keep-alive connections held about 200 MB before the fix and 1.87 MB after.
- Local RSS three minutes after load: 334 MB before, 65 MB after (30 MB idle).
- jemalloc without the fix: 364 MB against glibc's 334 MB. With the fix: glibc 65 MB, jemalloc
  with `thp:never` 87 MB.
- Docker Desktop's VM runs transparent huge pages in `always` mode.

## Checked in source

| Claim | Where |
|---|---|
| `Router<()>` as a make-service returns `self.clone().with_state(())` | axum 0.8.9 `src/routing/mod.rs:549-565` |
| `with_state` deep-clones `RouterInner` when the `Arc` is shared | axum 0.8.9 `src/routing/mod.rs:157-168` |
| `IntoMakeService` and `IntoMakeServiceWithConnectInfo` clone the already-built service | axum 0.8.9 `src/routing/into_make_service.rs:35`, `src/extract/connect_info.rs:128` |
| The handler is cloned per request | axum 0.8.9 `src/routing/method_routing.rs:1131` (`route.clone()`), `src/handler/service.rs:172` |
| `retain_recent`, `shrink_to_fit` on a keyed limiter | governor 0.10.4 `src/state/keyed.rs:232,243` |
| Maintenance runs lazily; `run_pending_tasks` forces it | moka 0.12.16 `src/lib.rs` "Maintenance Tasks", `src/future/cache.rs:1494` |
| `profiling` feature; `_RJEM_MALLOC_CONF` | tikv-jemallocator 0.7.0 `Cargo.toml`, tikv-jemalloc-sys 0.7.1 `README.md` |
| The build script runs `make` | tikv-jemalloc-sys 0.7.1 `build.rs:334` |
| `realloc` adds its size change to `bytes_allocated` or `bytes_deallocated`, so net growth is their difference; the incident report's formula also added `bytes_reallocated`, which counts it twice | stats_alloc 0.1.10 `src/lib.rs:270-282` |
| `prof_final`, `lg_prof_interval`, the `jeprof` prefix | jemalloc 5.3.1 `doc/jemalloc.xml.in`, bundled by tikv-jemalloc-sys 0.7.1 |
| `SwaggerUi` keeps the document in an `Arc` | utoipa-swagger-ui 10.0.2 `src/axum.rs` |

## Reproduced

The test in `rust-testing/references/connection-memory.md`, on the scaffold (macOS arm64,
axum 0.8.9, 50 keep-alive connections, net heap growth):

| Router | `into_make_service_with_connect_info` | bare `Router` |
|---|---|---|
| scaffold routes only | 1,417,600 bytes | 2,877,950 bytes |
| plus a route owning 1 MiB | 1,417,600 bytes | 55,306,750 bytes |

Net growth is `bytes_allocated - bytes_deallocated`. The reported 1.87 MB was computed with
`+ bytes_reallocated` and is likely high by the same double count (410,000 bytes here).

The scaffold's own routes capture too little to separate the two forms, which is why the recipe
adds the ballast route. The governor and moka pruning task in `operations.md` is a plain `rust`
fence (neither crate is in the stack); it was compiled and run once against governor 0.10.4 and
moka 0.12.16 outside the snippet run.

No eval has been run for the new rule or the two new eval cases.
