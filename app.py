from __future__ import annotations

import ctypes
import math
import os
import queue
import shutil
import sys
import threading
import time
import tkinter as tk
import tkinter.font as tkfont
import webbrowser
from concurrent.futures import ThreadPoolExecutor, as_completed
from ctypes import wintypes
from dataclasses import dataclass
from pathlib import Path
from tkinter import filedialog, messagebox
from urllib.parse import urlparse
from uuid import UUID


def show_dependency_error(package_name: str) -> None:
    root = tk.Tk()
    root.withdraw()
    messagebox.showerror(
        "依存パッケージが見つかりません",
        f"{package_name} がインストールされていません。\n\n"
        "先に `pip install -r requirements.txt` を実行してください。",
    )
    root.destroy()


try:
    import customtkinter as ctk
except ImportError:
    show_dependency_error("customtkinter")
    raise SystemExit(1)

try:
    import yt_dlp
    from yt_dlp.downloader.http import HttpFD
    from yt_dlp.networking import Request
    from yt_dlp.utils import parse_http_range
    from yt_dlp.utils.networking import HTTPHeaderDict
except ImportError:
    show_dependency_error("yt-dlp")
    raise SystemExit(1)


ctk.set_appearance_mode("system")
ctk.set_default_color_theme("blue")


APP_NAME = "Vidown"
APP_ID = "Vidown"
APP_ICON_FILENAME = "icon.ico"
PARALLEL_HTTP_WORKERS = 8
PARALLEL_HTTP_MIN_FILE_SIZE = 8 * 1024 * 1024
PARALLEL_HTTP_CHUNK_SIZE = 4 * 1024 * 1024
FRAGMENT_DOWNLOAD_WORKERS = 16
DOWNLOAD_BUFFER_SIZE = 1024 * 1024
NATIVE_HTTP_CHUNK_SIZE = 10 * 1024 * 1024
PROGRESS_EVENT_INTERVAL_SECONDS = 0.1


SUPPORTED_PLATFORMS = ("YouTube", "Twitter/X", "Instagram")
SUPPORTED_SITES_TEXT = " / ".join(SUPPORTED_PLATFORMS)
WINDOWS_DOWNLOADS_FOLDER_ID = "{374DE290-123F-4565-9164-39C4925E467B}"

BROWSER_LABELS = {
    "使用しない": None,
    "Chrome": "chrome",
    "Edge": "edge",
    "Firefox": "firefox",
}
CHROME_COOKIE_COPY_ERROR = "Could not copy Chrome cookie database"
COOKIE_EXPORT_EXTENSION_URL = "https://chromewebstore.google.com/detail/cclelndahbckbenkjhflpdbgdldlbecc?utm_source=item-share-cb"
COOKIE_FILE_DIALOG_TYPES = (
    ("Cookie files", "*.txt *.cookies"),
    ("All files", "*.*"),
)


@dataclass(frozen=True, slots=True)
class FormatChoice:
    choice_id: str
    selector: str
    title: str
    details: str
    format_id: str
    requires_merge: bool
    is_muxed: bool
    height: int
    sort_key: tuple[int, int, int, float, float]


@dataclass(slots=True)
class StreamProgress:
    downloaded_bytes: int = 0
    total_bytes: int | None = None
    total_is_estimate: bool = True
    speed: float = 0.0
    finished: bool = False


@dataclass(frozen=True, slots=True)
class TransferSnapshot:
    downloaded_bytes: int
    total_bytes: int | None
    total_is_estimate: bool
    progress: float | None
    speed: float
    eta: float | None
    all_finished: bool
    should_emit: bool


def nonnegative_int(value: object) -> int | None:
    try:
        result = int(value)  # type: ignore[arg-type]
    except (TypeError, ValueError, OverflowError):
        return None
    return result if result >= 0 else None


def positive_float(value: object) -> float | None:
    try:
        result = float(value)  # type: ignore[arg-type]
    except (TypeError, ValueError, OverflowError):
        return None
    return result if math.isfinite(result) and result > 0 else None


class DownloadProgressTracker:
    """Aggregate yt-dlp progress for all selected video/audio streams."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._streams: dict[str, StreamProgress] = {}
        self._format_keys: dict[str, list[str]] = {}
        self._last_emit_time = 0.0

    @staticmethod
    def _initial_total(fmt: dict[str, object]) -> tuple[int | None, bool]:
        exact = nonnegative_int(fmt.get("filesize"))
        if exact:
            return exact, False
        estimate = nonnegative_int(fmt.get("filesize_approx"))
        if estimate:
            return estimate, True
        return None, True

    def reset(self) -> None:
        with self._lock:
            self._streams.clear()
            self._format_keys.clear()
            self._last_emit_time = 0.0

    def prepare(self, info: dict[str, object]) -> TransferSnapshot:
        requested_formats = info.get("requested_formats")
        if isinstance(requested_formats, list) and requested_formats:
            formats = [fmt for fmt in requested_formats if isinstance(fmt, dict)]
        else:
            formats = [info]

        with self._lock:
            self._streams.clear()
            self._format_keys.clear()
            self._last_emit_time = 0.0

            for index, fmt in enumerate(formats):
                format_id = str(fmt.get("format_id") or "").strip()
                base_key = f"format:{format_id}" if format_id else f"stream:{index}"
                key = base_key
                duplicate_index = 2
                while key in self._streams:
                    key = f"{base_key}:{duplicate_index}"
                    duplicate_index += 1

                total, is_estimate = self._initial_total(fmt)
                self._streams[key] = StreamProgress(
                    total_bytes=total,
                    total_is_estimate=is_estimate,
                )
                if format_id:
                    self._format_keys.setdefault(format_id, []).append(key)

            return self._snapshot_locked(should_emit=True)

    def _find_stream_key_locked(
        self,
        data: dict[str, object],
        info: dict[str, object],
    ) -> str:
        format_id = str(info.get("format_id") or "").strip()
        matching_keys = self._format_keys.get(format_id, [])
        if matching_keys:
            return next(
                (key for key in matching_keys if not self._streams[key].finished),
                matching_keys[-1],
            )

        if len(self._streams) == 1:
            return next(iter(self._streams))

        filename = str(data.get("filename") or data.get("tmpfilename") or "").strip()
        identity = format_id or filename or str(data.get("ctx_id") or len(self._streams))
        key = f"observed:{identity}"
        if key not in self._streams:
            total, is_estimate = self._initial_total(info)
            self._streams[key] = StreamProgress(
                total_bytes=total,
                total_is_estimate=is_estimate,
            )
            if format_id:
                self._format_keys.setdefault(format_id, []).append(key)
        return key

    def update(self, data: dict[str, object]) -> TransferSnapshot:
        status = str(data.get("status") or "")
        raw_info = data.get("info_dict")
        info = raw_info if isinstance(raw_info, dict) else {}
        now = time.monotonic()

        with self._lock:
            key = self._find_stream_key_locked(data, info)
            stream = self._streams[key]

            downloaded = nonnegative_int(data.get("downloaded_bytes"))
            exact_total = nonnegative_int(data.get("total_bytes"))
            estimated_total = nonnegative_int(data.get("total_bytes_estimate"))

            if downloaded is not None:
                stream.downloaded_bytes = downloaded

            if exact_total:
                stream.total_bytes = exact_total
                stream.total_is_estimate = False
            elif estimated_total and (stream.total_bytes is None or stream.total_is_estimate):
                stream.total_bytes = estimated_total
                stream.total_is_estimate = True

            if stream.total_bytes is not None and stream.downloaded_bytes > stream.total_bytes:
                stream.total_bytes = stream.downloaded_bytes

            speed = positive_float(data.get("speed"))
            stream.speed = speed or 0.0

            if status == "finished":
                if downloaded is None:
                    if exact_total is not None:
                        stream.downloaded_bytes = exact_total
                    else:
                        filename = str(data.get("filename") or "")
                        try:
                            stream.downloaded_bytes = os.path.getsize(filename)
                        except OSError:
                            if stream.total_bytes is not None:
                                stream.downloaded_bytes = stream.total_bytes

                if stream.downloaded_bytes > 0:
                    stream.total_bytes = stream.downloaded_bytes
                    stream.total_is_estimate = False
                stream.speed = 0.0
                stream.finished = True

            force_emit = status in {"finished", "error"}
            should_emit = force_emit or now - self._last_emit_time >= PROGRESS_EVENT_INTERVAL_SECONDS
            if should_emit:
                self._last_emit_time = now
            return self._snapshot_locked(should_emit=should_emit)

    def _snapshot_locked(self, *, should_emit: bool) -> TransferSnapshot:
        streams = list(self._streams.values())
        downloaded = sum(stream.downloaded_bytes for stream in streams)
        all_totals_known = bool(streams) and all(stream.total_bytes is not None for stream in streams)
        total = (
            sum(stream.total_bytes or 0 for stream in streams)
            if all_totals_known
            else None
        )
        speed = sum(stream.speed for stream in streams if not stream.finished)
        all_finished = bool(streams) and all(stream.finished for stream in streams)

        if total and total > 0:
            progress = max(0.0, min(downloaded / total, 1.0))
            eta = max(total - downloaded, 0) / speed if speed > 0 else None
        elif all_finished:
            progress = 1.0
            eta = 0.0
            total = downloaded or None
        else:
            progress = None
            eta = None

        return TransferSnapshot(
            downloaded_bytes=downloaded,
            total_bytes=total,
            total_is_estimate=(
                not all_totals_known
                or any(stream.total_is_estimate for stream in streams)
            ),
            progress=progress,
            speed=speed,
            eta=eta,
            all_finished=all_finished,
            should_emit=should_emit,
        )


class ParallelHttpFD(HttpFD):
    """HTTP downloader using independent byte ranges with a safe native fallback."""

    def _request_extensions(self, info_dict: dict[str, object]) -> dict[str, object]:
        extensions: dict[str, object] = {}
        impersonate_target = self._get_impersonate_target(info_dict)
        if impersonate_target is not None:
            extensions["impersonate"] = impersonate_target
        return extensions

    def _probe_size(
        self,
        info_dict: dict[str, object],
        headers: HTTPHeaderDict,
    ) -> tuple[int, str | None] | None:
        probe_headers = HTTPHeaderDict(headers)
        probe_headers["Range"] = "bytes=0-0"
        request = Request(
            str(info_dict["url"]),
            info_dict.get("request_data"),
            probe_headers,
            extensions=self._request_extensions(info_dict),
        )
        response = None
        try:
            response = self.ydl.urlopen(request)
            range_start, _range_end, total = parse_http_range(
                response.headers.get("Content-Range")
            )
            if range_start != 0 or not total:
                return None
            return int(total), response.headers.get("Last-Modified")
        except Exception:
            return None
        finally:
            if response is not None:
                response.close()

    def real_download(self, filename, info_dict):
        if (
            not isinstance(filename, str)
            or filename == "-"
            or self.params.get("test")
            or self.params.get("nopart")
            or info_dict.get("request_data") is not None
        ):
            return super().real_download(filename, info_dict)

        native_tmpfilename = self.temp_name(filename)
        if os.path.exists(native_tmpfilename):
            # Preserve yt-dlp's reliable sequential resume behavior.
            return super().real_download(filename, info_dict)
        tmpfilename = f"{native_tmpfilename}.parallel"
        self.try_remove(tmpfilename)

        headers = HTTPHeaderDict(
            {"Accept-Encoding": "identity"},
            info_dict.get("http_headers"),
        )
        if headers.get("Range"):
            return super().real_download(filename, info_dict)

        probe = self._probe_size(info_dict, headers)
        if probe is None:
            return super().real_download(filename, info_dict)
        total_bytes, last_modified = probe
        if total_bytes < PARALLEL_HTTP_MIN_FILE_SIZE:
            return super().real_download(filename, info_dict)

        min_filesize = nonnegative_int(self.params.get("min_filesize"))
        max_filesize = nonnegative_int(self.params.get("max_filesize"))
        if (
            (min_filesize is not None and total_bytes < min_filesize)
            or (max_filesize is not None and total_bytes > max_filesize)
        ):
            return super().real_download(filename, info_dict)

        configured_workers = nonnegative_int(
            self.params.get("parallel_http_workers")
        ) or PARALLEL_HTTP_WORKERS
        worker_count = max(1, min(configured_workers, 32))
        target_range_size = max(
            PARALLEL_HTTP_CHUNK_SIZE,
            math.ceil(total_bytes / (worker_count * 4)),
        )
        ranges = [
            (start, min(start + target_range_size - 1, total_bytes - 1))
            for start in range(0, total_bytes, target_range_size)
        ]
        if len(ranges) < 2:
            return super().real_download(filename, info_dict)
        worker_count = min(worker_count, len(ranges))

        try:
            stream, tmpfilename = self.sanitize_open(tmpfilename, "w+b")
            try:
                stream.truncate(total_bytes)
            finally:
                stream.close()
            self.report_destination(filename)
        except OSError:
            self.try_remove(tmpfilename)
            return super().real_download(filename, info_dict)

        progress_lock = threading.Lock()
        stop_event = threading.Event()
        started_at = time.monotonic()
        downloaded_bytes = 0
        last_progress_at = 0.0
        buffer_size = nonnegative_int(self.params.get("buffersize")) or DOWNLOAD_BUFFER_SIZE
        retry_count = nonnegative_int(self.params.get("retries"))
        if retry_count is None:
            retry_count = 10

        def emit_progress(delta: int, *, force: bool = False) -> None:
            nonlocal downloaded_bytes, last_progress_at
            with progress_lock:
                downloaded_bytes += delta
                now = time.monotonic()
                if not force and now - last_progress_at < PROGRESS_EVENT_INTERVAL_SECONDS:
                    return
                last_progress_at = now
                current_downloaded = downloaded_bytes

            elapsed = max(now - started_at, 0.001)
            speed = current_downloaded / elapsed
            self._hook_progress(
                {
                    "status": "downloading",
                    "downloaded_bytes": current_downloaded,
                    "total_bytes": total_bytes,
                    "tmpfilename": tmpfilename,
                    "filename": filename,
                    "eta": max(total_bytes - current_downloaded, 0) / speed,
                    "speed": speed,
                    "elapsed": elapsed,
                    "ctx_id": info_dict.get("ctx_id"),
                },
                info_dict,
            )

        def download_range(range_start: int, range_end: int) -> None:
            position = range_start
            attempts = 0

            with open(tmpfilename, "r+b", buffering=0) as output:
                while position <= range_end and not stop_event.is_set():
                    range_headers = HTTPHeaderDict(headers)
                    range_headers["Range"] = f"bytes={position}-{range_end}"
                    request = Request(
                        str(info_dict["url"]),
                        info_dict.get("request_data"),
                        range_headers,
                        extensions=self._request_extensions(info_dict),
                    )
                    response = None
                    try:
                        response = self.ydl.urlopen(request)
                        response_start, response_end, response_total = parse_http_range(
                            response.headers.get("Content-Range")
                        )
                        if (
                            response_start != position
                            or response_end is None
                            or response_end > range_end
                            or response_total != total_bytes
                        ):
                            raise OSError("サーバーが要求したバイト範囲を返しませんでした")

                        output.seek(position)
                        remaining = response_end - position + 1
                        while remaining > 0 and not stop_event.is_set():
                            block = response.read(min(buffer_size, remaining))
                            if not block:
                                raise OSError("バイト範囲の受信が途中で終了しました")
                            output.write(block)
                            position += len(block)
                            remaining -= len(block)
                            emit_progress(len(block))
                        attempts = 0
                    except Exception:
                        attempts += 1
                        if attempts > retry_count or stop_event.is_set():
                            raise
                        time.sleep(min(0.25 * (2 ** (attempts - 1)), 2.0))
                    finally:
                        if response is not None:
                            response.close()

        failures: list[Exception] = []
        try:
            with ThreadPoolExecutor(
                max_workers=worker_count,
                thread_name_prefix="vidown-http",
            ) as executor:
                futures = [
                    executor.submit(download_range, range_start, range_end)
                    for range_start, range_end in ranges
                ]
                for future in as_completed(futures):
                    try:
                        future.result()
                    except Exception as exc:
                        failures.append(exc)
                        stop_event.set()
                        for pending in futures:
                            pending.cancel()
                        break
        except Exception as exc:
            failures.append(exc)
            stop_event.set()

        if failures:
            self.try_remove(tmpfilename)
            self.report_warning(
                "並列HTTPダウンロードを継続できないため、標準方式で再試行します。"
            )
            return super().real_download(filename, info_dict)

        emit_progress(0, force=True)
        self.try_rename(tmpfilename, filename)
        if self.params.get("updatetime"):
            info_dict["filetime"] = self.try_utime(filename, last_modified)

        elapsed = max(time.monotonic() - started_at, 0.0)
        self._hook_progress(
            {
                "status": "finished",
                "downloaded_bytes": total_bytes,
                "total_bytes": total_bytes,
                "filename": filename,
                "elapsed": elapsed,
                "ctx_id": info_dict.get("ctx_id"),
            },
            info_dict,
        )
        return True


class FastYoutubeDL(yt_dlp.YoutubeDL):
    def dl(self, name, info, subtitle=False, test=False):
        protocol = str(info.get("protocol") or "")
        url = str(info.get("url") or "")
        can_use_parallel_http = (
            not subtitle
            and not test
            and isinstance(name, str)
            and name != "-"
            and protocol in {"http", "https"}
            and urlparse(url).scheme in {"http", "https"}
            and not info.get("fragments")
            and not info.get("requested_formats")
            and not info.get("section_start")
            and not info.get("section_end")
        )
        if not can_use_parallel_http:
            return super().dl(name, info, subtitle=subtitle, test=test)

        downloader = ParallelHttpFD(self, self.params)
        for progress_hook in self._progress_hooks:
            downloader.add_progress_hook(progress_hook)

        new_info = self._copy_infodict(info)
        if new_info.get("http_headers") is None:
            new_info["http_headers"] = self._calc_headers(new_info)
        self.write_debug(f"Invoking {downloader.FD_NAME} downloader on a direct HTTP stream")
        return downloader.download(name, new_info, subtitle)


class GUID(ctypes.Structure):
    _fields_ = [
        ("Data1", wintypes.DWORD),
        ("Data2", wintypes.WORD),
        ("Data3", wintypes.WORD),
        ("Data4", ctypes.c_ubyte * 8),
    ]

    def __init__(self, value: str) -> None:
        super().__init__()
        uuid_value = UUID(value)
        self.Data1, self.Data2, self.Data3 = uuid_value.fields[:3]
        for index, byte in enumerate(uuid_value.bytes[8:]):
            self.Data4[index] = byte


def get_windows_known_folder(folder_id: str) -> Path:
    path_ptr = ctypes.c_wchar_p()
    guid = GUID(folder_id)
    result = ctypes.windll.shell32.SHGetKnownFolderPath(
        ctypes.byref(guid),
        0,
        None,
        ctypes.byref(path_ptr),
    )
    if result != 0:
        raise OSError(f"SHGetKnownFolderPath failed with HRESULT {result}")

    try:
        return Path(path_ptr.value)
    finally:
        ctypes.windll.ole32.CoTaskMemFree(path_ptr)


def get_downloads_directory() -> Path:
    if os.name == "nt":
        try:
            downloads_dir = get_windows_known_folder(WINDOWS_DOWNLOADS_FOLDER_ID)
            if downloads_dir:
                return downloads_dir
        except OSError:
            pass
    return Path.home() / "Downloads"


def get_resource_path(name: str) -> Path:
    bundle_dir = getattr(sys, "_MEIPASS", None)
    if bundle_dir:
        return Path(bundle_dir) / name
    return Path(__file__).resolve().with_name(name)


def set_windows_app_id(app_id: str) -> None:
    if os.name != "nt":
        return

    try:
        ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID(app_id)
    except (AttributeError, OSError):
        pass


def detect_platform(url: str) -> str | None:
    try:
        parsed = urlparse(url.strip())
    except ValueError:
        return None

    if parsed.scheme not in {"http", "https"}:
        return None

    host = parsed.netloc.lower()
    if host.startswith("www."):
        host = host[4:]

    if host == "youtu.be" or host.endswith("youtube.com"):
        return "YouTube"
    if host.endswith("twitter.com") or host.endswith("x.com"):
        return "Twitter/X"
    if host.endswith("instagram.com"):
        return "Instagram"
    return None


def human_readable_size(num_bytes: float | int | None) -> str:
    if not num_bytes:
        return "0 B"

    size = float(num_bytes)
    units = ["B", "KB", "MB", "GB", "TB"]
    for unit in units:
        if size < 1024 or unit == units[-1]:
            return f"{size:.1f} {unit}"
        size /= 1024
    return f"{size:.1f} TB"


def display_file_size(num_bytes: float | int | None) -> str:
    if not num_bytes:
        return "サイズ不明"
    return human_readable_size(num_bytes)


def format_eta(seconds: int | float | None) -> str:
    if seconds is None:
        return "--:--"

    total_seconds = max(int(seconds), 0)
    minutes, remaining_seconds = divmod(total_seconds, 60)
    hours, remaining_minutes = divmod(minutes, 60)

    if hours:
        return f"{hours:02d}:{remaining_minutes:02d}:{remaining_seconds:02d}"
    return f"{remaining_minutes:02d}:{remaining_seconds:02d}"


def detect_js_runtime() -> tuple[str | None, str | None]:
    candidates = (
        ("node", ("node",)),
        ("deno", ("deno",)),
        ("quickjs", ("qjs", "quickjs")),
        ("bun", ("bun",)),
    )

    for runtime_name, commands in candidates:
        for command in commands:
            runtime_path = shutil.which(command)
            if runtime_path:
                return runtime_name, runtime_path
    return None, None


def pick_font_family(candidates: tuple[str, ...], installed_fonts: set[str]) -> str:
    for family in candidates:
        if family in installed_fonts:
            return family
    return "TkDefaultFont"


def codec_label(codec: object) -> str | None:
    if codec in {None, "none"}:
        return None
    return str(codec).split(".")[0]


def is_video_format(fmt: dict[str, object]) -> bool:
    if fmt.get("vcodec") == "none":
        return False
    return bool(fmt.get("height") or fmt.get("width") or fmt.get("vcodec") not in {None, "none"})


def looks_like_muxed_format(fmt: dict[str, object], platform: str) -> bool:
    acodec = fmt.get("acodec")
    if acodec not in {None, "none"}:
        return True

    protocol = str(fmt.get("protocol") or "")
    format_id = str(fmt.get("format_id") or "")
    if platform in {"Twitter/X", "Instagram"} and (protocol == "https" or format_id.startswith("http-")):
        return True

    return False


def build_selector_for_format(
    fmt: dict[str, object],
    platform: str,
    ffmpeg_available: bool,
) -> tuple[str | None, bool, bool]:
    format_id = str(fmt.get("format_id") or "").strip()
    if not format_id or not is_video_format(fmt):
        return None, False, False

    is_muxed = looks_like_muxed_format(fmt, platform)
    if is_muxed:
        return format_id, True, False

    if not ffmpeg_available:
        return None, False, False

    selector = (
        f"{format_id}+bestaudio[ext=m4a][acodec!=none]"
        f"/{format_id}+bestaudio[acodec!=none]"
        f"/{format_id}+bestaudio"
    )
    return selector, False, True


def build_format_choice(
    fmt: dict[str, object],
    platform: str,
    ffmpeg_available: bool,
    index: int,
) -> FormatChoice | None:
    selector, is_muxed, requires_merge = build_selector_for_format(fmt, platform, ffmpeg_available)
    if not selector:
        return None

    format_id = str(fmt.get("format_id") or "")
    ext = str(fmt.get("ext") or "unknown")
    height = int(fmt.get("height") or 0)
    fps = float(fmt.get("fps") or 0.0)
    tbr = float(fmt.get("tbr") or 0.0)
    size = fmt.get("filesize") or fmt.get("filesize_approx")
    protocol = str(fmt.get("protocol") or "unknown").replace("_native", "").upper()
    resolution = f"{height}p" if height else str(fmt.get("resolution") or "不明")
    fps_label = f"{int(round(fps))}fps" if fps >= 1 else None
    note = str(fmt.get("format_note") or "").strip() or None
    vcodec = codec_label(fmt.get("vcodec"))
    audio_state = "音声入り" if is_muxed else "映像と音声を結合"

    title_parts = [resolution, ext.upper(), audio_state, f"id={format_id}"]
    detail_parts = [protocol, display_file_size(size)]
    if fps_label:
        detail_parts.append(fps_label)
    if note:
        detail_parts.append(note)
    if vcodec:
        detail_parts.append(vcodec)

    return FormatChoice(
        choice_id=f"format_{index}",
        selector=selector,
        title=" | ".join(title_parts),
        details=" / ".join(detail_parts),
        format_id=format_id,
        requires_merge=requires_merge,
        is_muxed=is_muxed,
        height=height,
        sort_key=(height, 1 if is_muxed else 0, 1 if ext.lower() == "mp4" else 0, fps, tbr),
    )


def build_format_choices(
    info: dict[str, object],
    platform: str,
    ffmpeg_available: bool,
) -> list[FormatChoice]:
    raw_formats = info.get("formats")
    if not isinstance(raw_formats, list):
        return []

    choices: list[FormatChoice] = []
    seen_selectors: set[str] = set()
    muxed_keys: set[tuple[int, str]] = set()

    for index, fmt in enumerate(raw_formats):
        if not isinstance(fmt, dict):
            continue
        choice = build_format_choice(fmt, platform, ffmpeg_available, index)
        if not choice or not choice.is_muxed or choice.selector in seen_selectors:
            continue
        choices.append(choice)
        seen_selectors.add(choice.selector)
        muxed_keys.add((choice.height, str(fmt.get("ext") or "")))

    for index, fmt in enumerate(raw_formats):
        if not isinstance(fmt, dict):
            continue
        choice = build_format_choice(fmt, platform, ffmpeg_available, index)
        if not choice or choice.is_muxed or choice.selector in seen_selectors:
            continue
        if platform in {"Twitter/X", "Instagram"} and (choice.height, str(fmt.get("ext") or "")) in muxed_keys:
            continue
        choices.append(choice)
        seen_selectors.add(choice.selector)

    choices.sort(key=lambda item: item.sort_key, reverse=True)
    return choices


def describe_selected_format(choice: FormatChoice) -> str:
    return f"{choice.title} ({choice.details})"


class YtdlpLogger:
    def __init__(self, emit_event) -> None:
        self.emit_event = emit_event

    def debug(self, message: str) -> None:
        if message.startswith("[debug] "):
            return
        self.info(message)

    def info(self, message: str) -> None:
        clean = message.strip()
        if clean:
            self.emit_event({"type": "log", "message": clean})

    def warning(self, message: str) -> None:
        clean = message.strip()
        if clean:
            self.emit_event({"type": "log", "message": f"Warning: {clean}"})

    def error(self, message: str) -> None:
        clean = message.strip()
        if clean:
            self.emit_event({"type": "log", "message": f"Error: {clean}"})


def format_download_error(exc: Exception) -> str:
    message = str(exc)
    if CHROME_COOKIE_COPY_ERROR in message:
        return (
            "Chrome/EdgeのCookieデータベースをコピーできませんでした。"
            " Chrome/Edgeを完全に終了して再試行するか、Netscape形式のcookies.txtをCookieファイルに指定してください。"
            " 年齢制限付きYouTubeはログイン済みアカウントのCookieが必要です。"
        )
    return message


class FormatSelectionDialog(ctk.CTkToplevel):
    def __init__(
        self,
        parent: "VideoDownloaderApp",
        title_text: str,
        platform: str,
        choices: list[FormatChoice],
        on_confirm,
        on_cancel,
    ) -> None:
        super().__init__(parent)
        self.title("画質を選択")
        self.geometry("780x560")
        self.minsize(700, 500)
        self.transient(parent)
        self.grab_set()

        self.on_confirm = on_confirm
        self.on_cancel = on_cancel
        self.choice_map = {choice.choice_id: choice for choice in choices}
        self.selected_choice_id = tk.StringVar(value=choices[0].choice_id)
        self._confirmed = False

        self.protocol("WM_DELETE_WINDOW", self.handle_cancel)
        self.grid_columnconfigure(0, weight=1)
        self.grid_rowconfigure(2, weight=1)

        header = ctk.CTkFrame(self, corner_radius=18)
        header.grid(row=0, column=0, sticky="ew", padx=20, pady=(20, 12))
        header.grid_columnconfigure(0, weight=1)

        title_label = ctk.CTkLabel(
            header,
            text="利用可能な画質を選択してください",
            font=parent.title_font,
        )
        title_label.grid(row=0, column=0, sticky="w", padx=18, pady=(16, 4))

        subtitle = ctk.CTkLabel(
            header,
            text=f"{platform} / {title_text}",
            font=parent.subtitle_font,
            text_color=("gray35", "gray70"),
            wraplength=700,
            justify="left",
        )
        subtitle.grid(row=1, column=0, sticky="w", padx=18, pady=(0, 16))

        note = ctk.CTkLabel(
            self,
            text="音声入りの形式を優先して並べています。映像と音声を結合する形式は ffmpeg を使います。",
            font=parent.subtitle_font,
            text_color=("gray35", "gray70"),
            wraplength=720,
            justify="left",
        )
        note.grid(row=1, column=0, sticky="w", padx=22)

        scroll = ctk.CTkScrollableFrame(self, corner_radius=16)
        scroll.grid(row=2, column=0, sticky="nsew", padx=20, pady=(12, 12))
        scroll.grid_columnconfigure(0, weight=1)

        for row_index, choice in enumerate(choices):
            row = ctk.CTkFrame(scroll, fg_color="transparent")
            row.grid(row=row_index, column=0, sticky="ew", padx=8, pady=6)
            row.grid_columnconfigure(0, weight=1)

            radio = ctk.CTkRadioButton(
                row,
                text=choice.title,
                value=choice.choice_id,
                variable=self.selected_choice_id,
                font=parent.ui_font,
            )
            radio.grid(row=0, column=0, sticky="w")

            details = ctk.CTkLabel(
                row,
                text=choice.details,
                font=parent.subtitle_font,
                text_color=("gray35", "gray70"),
                wraplength=650,
                justify="left",
            )
            details.grid(row=1, column=0, sticky="w", padx=(30, 0), pady=(2, 0))

        buttons = ctk.CTkFrame(self, fg_color="transparent")
        buttons.grid(row=3, column=0, sticky="ew", padx=20, pady=(0, 20))
        buttons.grid_columnconfigure(0, weight=1)

        cancel_button = ctk.CTkButton(
            buttons,
            text="キャンセル",
            width=140,
            fg_color="transparent",
            border_width=1,
            text_color=("gray10", "gray90"),
            font=parent.button_font,
            command=self.handle_cancel,
        )
        cancel_button.grid(row=0, column=0, sticky="w")

        save_button = ctk.CTkButton(
            buttons,
            text="この形式で保存",
            width=170,
            font=parent.button_font,
            command=self.handle_confirm,
        )
        save_button.grid(row=0, column=1, sticky="e")

    def handle_confirm(self) -> None:
        choice = self.choice_map.get(self.selected_choice_id.get())
        if not choice:
            return
        self._confirmed = True
        self.grab_release()
        self.destroy()
        self.on_confirm(choice)

    def handle_cancel(self) -> None:
        if self._confirmed:
            return
        self.grab_release()
        self.destroy()
        self.on_cancel()


class VideoDownloaderApp(ctk.CTk):
    def __init__(self) -> None:
        super().__init__()
        set_windows_app_id(APP_ID)
        self.title(APP_NAME)
        self.geometry("880x740")
        self.minsize(520, 420)

        installed_fonts = set(tkfont.families())
        body_family = pick_font_family(
            ("Yu Gothic UI", "Segoe UI Variable Text", "Segoe UI", "Meiryo UI"),
            installed_fonts,
        )
        title_family = pick_font_family(
            ("Segoe UI Variable Display", "Bahnschrift", body_family),
            installed_fonts,
        )

        self.title_font = ctk.CTkFont(family=title_family, size=31, weight="bold")
        self.subtitle_font = ctk.CTkFont(family=body_family, size=14)
        self.label_font = ctk.CTkFont(family=body_family, size=13, weight="bold")
        self.ui_font = ctk.CTkFont(family=body_family, size=14)
        self.button_font = ctk.CTkFont(family=body_family, size=14, weight="bold")
        self.status_font = ctk.CTkFont(family=body_family, size=14)

        downloads_dir = get_downloads_directory()

        self.url_var = tk.StringVar()
        self.folder_var = tk.StringVar(value=str(downloads_dir))
        self.browser_var = tk.StringVar(value="使用しない")
        self.cookie_file_var = tk.StringVar()
        self.platform_var = tk.StringVar(value=f"対応サイト: {SUPPORTED_SITES_TEXT}")
        self.status_var = tk.StringVar(value="URLを入力してダウンロードを開始してください。")

        self.event_queue: queue.Queue[dict[str, object]] = queue.Queue()
        self.is_busy = False
        self.format_dialog: FormatSelectionDialog | None = None
        self.progress_tracker = DownloadProgressTracker()
        self.progress_value = 0.0
        self.progress_is_indeterminate = False

        self.url_var.trace_add("write", self.on_url_changed)

        self.build_ui()
        self.apply_window_icon()
        self.after(100, self.process_events)

    def build_ui(self) -> None:
        self.grid_columnconfigure(0, weight=1)
        self.grid_rowconfigure(0, weight=1)

        scroll_area = ctk.CTkFrame(self, fg_color="transparent")
        scroll_area.grid(row=0, column=0, sticky="nsew")
        scroll_area.grid_columnconfigure(0, weight=1)
        scroll_area.grid_rowconfigure(0, weight=1)

        canvas_bg = self._apply_appearance_mode(self.cget("fg_color"))
        self.scroll_canvas = tk.Canvas(
            scroll_area,
            bg=canvas_bg,
            borderwidth=0,
            highlightthickness=0,
        )
        self.scroll_canvas.grid(row=0, column=0, sticky="nsew")

        y_scrollbar = ctk.CTkScrollbar(
            scroll_area,
            orientation="vertical",
            command=self.scroll_canvas.yview,
        )
        y_scrollbar.grid(row=0, column=1, sticky="ns")

        x_scrollbar = ctk.CTkScrollbar(
            scroll_area,
            orientation="horizontal",
            command=self.scroll_canvas.xview,
        )
        x_scrollbar.grid(row=1, column=0, sticky="ew")

        self.scroll_canvas.configure(
            xscrollcommand=x_scrollbar.set,
            yscrollcommand=y_scrollbar.set,
        )

        scroll_content = ctk.CTkFrame(self.scroll_canvas, fg_color="transparent")
        scroll_content.grid_columnconfigure(0, weight=1)
        scroll_window = self.scroll_canvas.create_window(
            (0, 0),
            window=scroll_content,
            anchor="nw",
        )

        def update_scroll_region(_event=None) -> None:
            self.scroll_canvas.configure(scrollregion=self.scroll_canvas.bbox("all"))

        def resize_scroll_content(event) -> None:
            width = max(scroll_content.winfo_reqwidth(), event.width)
            self.scroll_canvas.itemconfigure(scroll_window, width=width)
            update_scroll_region()

        scroll_content.bind("<Configure>", update_scroll_region)
        self.scroll_canvas.bind("<Configure>", resize_scroll_content)
        self.bind_all("<MouseWheel>", self.on_mousewheel, add="+")

        container = ctk.CTkFrame(scroll_content, corner_radius=20)
        container.grid(row=0, column=0, sticky="nsew", padx=24, pady=24)
        container.grid_columnconfigure(0, weight=1)

        title = ctk.CTkLabel(
            container,
            text=APP_NAME,
            font=self.title_font,
        )
        title.grid(row=0, column=0, sticky="w", padx=24, pady=(24, 4))

        subtitle = ctk.CTkLabel(
            container,
            text="開始後に利用可能な画質一覧を取得し、選んだ形式で動画を保存します。",
            text_color=("gray25", "gray75"),
            font=self.subtitle_font,
        )
        subtitle.grid(row=1, column=0, sticky="w", padx=24, pady=(0, 18))

        url_label = ctk.CTkLabel(container, text="動画URL", font=self.label_font)
        url_label.grid(row=2, column=0, sticky="w", padx=24)

        url_frame = ctk.CTkFrame(container, fg_color="transparent")
        url_frame.grid(row=3, column=0, sticky="ew", padx=24, pady=(8, 6))
        url_frame.grid_columnconfigure(0, weight=1)

        self.url_entry = ctk.CTkEntry(
            url_frame,
            textvariable=self.url_var,
            placeholder_text="https://www.youtube.com/watch?v=... / https://x.com/... / https://www.instagram.com/...",
            height=42,
            font=self.ui_font,
        )
        self.url_entry.grid(row=0, column=0, sticky="ew")
        self.url_entry.bind("<Return>", self.start_download)

        platform_label = ctk.CTkLabel(
            container,
            textvariable=self.platform_var,
            text_color=("gray35", "gray70"),
            font=self.subtitle_font,
        )
        platform_label.grid(row=4, column=0, sticky="w", padx=24, pady=(0, 18))

        settings_frame = ctk.CTkFrame(container, corner_radius=16)
        settings_frame.grid(row=5, column=0, sticky="ew", padx=24, pady=(0, 16))
        settings_frame.grid_columnconfigure(0, weight=1)

        folder_label = ctk.CTkLabel(settings_frame, text="保存先フォルダ", font=self.label_font)
        folder_label.grid(row=0, column=0, sticky="w", padx=16, pady=(16, 6))

        folder_row = ctk.CTkFrame(settings_frame, fg_color="transparent")
        folder_row.grid(row=1, column=0, sticky="ew", padx=16, pady=(0, 14))
        folder_row.grid_columnconfigure(0, weight=1)

        folder_entry = ctk.CTkEntry(
            folder_row,
            textvariable=self.folder_var,
            height=40,
            font=self.ui_font,
        )
        folder_entry.grid(row=0, column=0, sticky="ew", padx=(0, 10))

        browse_button = ctk.CTkButton(
            folder_row,
            text="参照",
            width=100,
            command=self.choose_folder,
            font=self.button_font,
        )
        browse_button.grid(row=0, column=1)

        browser_label = ctk.CTkLabel(settings_frame, text="ブラウザCookie", font=self.label_font)
        browser_label.grid(row=2, column=0, sticky="w", padx=16, pady=(0, 6))

        browser_menu = ctk.CTkOptionMenu(
            settings_frame,
            variable=self.browser_var,
            values=list(BROWSER_LABELS.keys()),
            height=40,
            font=self.ui_font,
            dropdown_font=self.ui_font,
            command=self.on_browser_cookie_changed,
        )
        browser_menu.grid(row=3, column=0, sticky="ew", padx=16, pady=(0, 14))

        self.cookie_file_label = ctk.CTkLabel(settings_frame, text="Cookieファイル（必須）", font=self.label_font)
        self.cookie_file_label.grid(row=4, column=0, sticky="w", padx=16, pady=(0, 6))

        self.cookie_file_row = ctk.CTkFrame(settings_frame, fg_color="transparent")
        self.cookie_file_row.grid(row=5, column=0, sticky="ew", padx=16, pady=(0, 14))
        self.cookie_file_row.grid_columnconfigure(0, weight=1)

        cookie_file_entry = ctk.CTkEntry(
            self.cookie_file_row,
            textvariable=self.cookie_file_var,
            placeholder_text="Netscape形式の cookies.txt を指定してください",
            height=40,
            font=self.ui_font,
        )
        cookie_file_entry.grid(row=0, column=0, sticky="ew", padx=(0, 10))

        cookie_browse_button = ctk.CTkButton(
            self.cookie_file_row,
            text="参照",
            width=86,
            command=self.choose_cookie_file,
            font=self.button_font,
        )
        cookie_browse_button.grid(row=0, column=1, padx=(0, 8))

        cookie_clear_button = ctk.CTkButton(
            self.cookie_file_row,
            text="解除",
            width=72,
            fg_color="transparent",
            border_width=1,
            text_color=("gray10", "gray90"),
            command=lambda: self.cookie_file_var.set(""),
            font=self.button_font,
        )
        cookie_clear_button.grid(row=0, column=2)

        cookie_extension_button = ctk.CTkButton(
            self.cookie_file_row,
            text="Cookie出力拡張機能を開く",
            width=190,
            height=40,
            command=self.open_cookie_export_extension,
            font=self.button_font,
        )
        cookie_extension_button.grid(row=0, column=3, padx=(8, 0))
        self.update_cookie_file_visibility()

        action_row = ctk.CTkFrame(container, fg_color="transparent")
        action_row.grid(row=6, column=0, sticky="ew", padx=24)
        action_row.grid_columnconfigure(0, weight=1)

        self.download_button = ctk.CTkButton(
            action_row,
            text="ダウンロード開始",
            height=44,
            command=self.start_download,
            font=self.button_font,
        )
        self.download_button.grid(row=0, column=0, sticky="ew", padx=(0, 12))

        open_button = ctk.CTkButton(
            action_row,
            text="保存先を開く",
            width=150,
            fg_color="transparent",
            border_width=1,
            text_color=("gray10", "gray90"),
            command=self.open_output_folder,
            font=self.button_font,
        )
        open_button.grid(row=0, column=1)

        self.progress_bar = ctk.CTkProgressBar(container)
        self.progress_bar.grid(row=7, column=0, sticky="ew", padx=24, pady=(18, 8))
        self.reset_progress_tracking()

        status_label = ctk.CTkLabel(
            container,
            textvariable=self.status_var,
            anchor="w",
            font=self.status_font,
        )
        status_label.grid(row=8, column=0, sticky="new", padx=24, pady=(0, 24))

    def is_pointer_over_scroll_area(self) -> bool:
        canvas = getattr(self, "scroll_canvas", None)
        if canvas is None:
            return False

        pointer_x = canvas.winfo_pointerx()
        pointer_y = canvas.winfo_pointery()
        canvas_x = canvas.winfo_rootx()
        canvas_y = canvas.winfo_rooty()
        return (
            canvas_x <= pointer_x <= canvas_x + canvas.winfo_width()
            and canvas_y <= pointer_y <= canvas_y + canvas.winfo_height()
        )

    def on_mousewheel(self, event) -> None:
        if not self.is_pointer_over_scroll_area():
            return

        steps = -int(event.delta / 120)
        if steps == 0:
            steps = -1 if event.delta > 0 else 1

        if event.state & 0x0001:
            self.scroll_canvas.xview_scroll(steps, "units")
        else:
            self.scroll_canvas.yview_scroll(steps, "units")

    def on_browser_cookie_changed(self, _selected: str | None = None) -> None:
        self.update_cookie_file_visibility()

    def update_cookie_file_visibility(self) -> None:
        cookie_enabled = BROWSER_LABELS.get(self.browser_var.get()) is not None
        if cookie_enabled:
            self.cookie_file_label.grid()
            self.cookie_file_row.grid()
        else:
            self.cookie_file_label.grid_remove()
            self.cookie_file_row.grid_remove()

        canvas = getattr(self, "scroll_canvas", None)
        if canvas is not None:
            self.after_idle(lambda: canvas.configure(scrollregion=canvas.bbox("all")))

    def apply_window_icon(self) -> None:
        icon_path = get_resource_path(APP_ICON_FILENAME)
        if not icon_path.exists():
            return

        try:
            self.iconbitmap(default=str(icon_path))
        except tk.TclError:
            pass

    def reset_progress_tracking(self) -> None:
        self.progress_tracker.reset()
        self.set_progress(0.0, immediate=True)

    def start_progress_tracking(self, _choice: FormatChoice) -> None:
        self.progress_tracker.reset()
        self.set_progress(0.0, immediate=True)

    def set_progress(self, value: float, *, immediate: bool = False) -> None:
        clamped = max(0.0, min(value, 1.0))
        if self.progress_is_indeterminate:
            self.progress_bar.stop()
            self.progress_bar.configure(mode="determinate")
            self.progress_is_indeterminate = False
        self.progress_value = clamped
        self.progress_bar.set(clamped)

    def set_indeterminate_progress(self) -> None:
        if self.progress_is_indeterminate:
            return
        self.progress_bar.configure(mode="indeterminate")
        self.progress_bar.start()
        self.progress_is_indeterminate = True

    def make_ydl_options(
        self,
        platform: str,
        browser_name: str | None,
        cookie_file_path: Path | None = None,
        output_dir: Path | None = None,
        format_selector: str | None = None,
        include_progress: bool = False,
    ) -> tuple[dict[str, object], bool]:
        ffmpeg_available = shutil.which("ffmpeg") is not None
        ydl_opts: dict[str, object] = {
            "noplaylist": True,
            "windowsfilenames": True,
            "logger": YtdlpLogger(self.emit_event),
            "retries": 10,
            "fragment_retries": 10,
            "extractor_retries": 5,
            "file_access_retries": 3,
            "socket_timeout": 30,
            "buffersize": DOWNLOAD_BUFFER_SIZE,
            "noresizebuffer": True,
            "http_chunk_size": NATIVE_HTTP_CHUNK_SIZE,
            "concurrent_fragment_downloads": FRAGMENT_DOWNLOAD_WORKERS,
            "parallel_http_workers": PARALLEL_HTTP_WORKERS,
            "hls_prefer_native": True,
            "quiet": True,
            "noprogress": True,
            "no_warnings": False,
        }

        if output_dir is not None:
            ydl_opts["outtmpl"] = str(output_dir / "%(title).200B [%(id)s].%(ext)s")

        if include_progress:
            ydl_opts["progress_hooks"] = [self.on_progress]

        if ffmpeg_available:
            ydl_opts["merge_output_format"] = "mp4/mkv"

        if format_selector:
            ydl_opts["format"] = format_selector

        if cookie_file_path is not None:
            ydl_opts["cookiefile"] = str(cookie_file_path)
        elif browser_name:
            ydl_opts["cookiesfrombrowser"] = (browser_name, None, None, None)

        if platform == "YouTube":
            runtime_name, runtime_path = detect_js_runtime()
            if runtime_name and runtime_path:
                ydl_opts["js_runtimes"] = {runtime_name: {"path": runtime_path}}
                ydl_opts["remote_components"] = ["ejs:github"]
            else:
                self.emit_event(
                    {
                        "type": "log",
                        "message": (
                            "YouTube 抽出用の JavaScript ランタイムが見つかりません。"
                            " Node.js か Deno がない環境では一部動画が失敗する場合があります。"
                        ),
                    }
                )

        return ydl_opts, ffmpeg_available

    def choose_folder(self) -> None:
        initial_dir = self.folder_var.get() or str(get_downloads_directory())
        selected = filedialog.askdirectory(initialdir=initial_dir)
        if selected:
            self.folder_var.set(selected)

    def choose_cookie_file(self) -> None:
        selected = filedialog.askopenfilename(
            title="Cookieファイルを選択",
            filetypes=COOKIE_FILE_DIALOG_TYPES,
        )
        if selected:
            self.cookie_file_var.set(selected)

    def open_cookie_export_extension(self) -> None:
        webbrowser.open(COOKIE_EXPORT_EXTENSION_URL)

    def open_output_folder(self) -> None:
        path = Path(self.folder_var.get()).expanduser()
        if path.exists():
            os.startfile(path.resolve())
            return
        messagebox.showwarning("保存先が見つかりません", "指定された保存先フォルダが存在しません。")

    def on_url_changed(self, *_args) -> None:
        platform = detect_platform(self.url_var.get())
        if platform:
            self.platform_var.set(f"検出サイト: {platform}")
        else:
            self.platform_var.set(f"対応サイト: {SUPPORTED_SITES_TEXT}")

    def start_download(self, _event=None) -> None:
        if self.is_busy:
            return

        url = self.url_var.get().strip()
        platform = detect_platform(url)
        if not platform:
            messagebox.showwarning(
                "未対応のURLです",
                "YouTube / Twitter(X) / Instagram のURLを入力してください。",
            )
            return

        output_dir = Path(self.folder_var.get()).expanduser()
        try:
            output_dir.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            messagebox.showerror("保存先を作成できません", str(exc))
            return

        if self.format_dialog is not None:
            self.format_dialog.destroy()
            self.format_dialog = None

        browser_name = BROWSER_LABELS.get(self.browser_var.get())
        cookie_file_path: Path | None = None
        if browser_name:
            cookie_file_text = self.cookie_file_var.get().strip()
            if not cookie_file_text:
                messagebox.showwarning(
                    "Cookieファイルが必要です",
                    "ブラウザCookieを使用するにはCookieファイルを指定してください。",
                )
                return

            cookie_file_path = Path(cookie_file_text).expanduser()
            if not cookie_file_path.is_file():
                messagebox.showwarning(
                    "Cookieファイルが見つかりません",
                    "指定されたCookieファイルが存在しません。",
                )
                return

        self.is_busy = True
        self.reset_progress_tracking()
        self.status_var.set(f"{platform} の利用可能な画質を取得しています...")
        self.download_button.configure(state="disabled")
        self.append_log("")
        self.append_log(f"URL: {url}")
        self.append_log(f"保存先: {output_dir}")
        self.append_log(f"サイト: {platform}")
        if cookie_file_path:
            self.append_log(f"Cookieファイル: {cookie_file_path}")

        worker = threading.Thread(
            target=self.fetch_available_formats,
            args=(url, output_dir, browser_name, cookie_file_path, platform),
            daemon=True,
        )
        worker.start()

    def fetch_available_formats(
        self,
        url: str,
        output_dir: Path,
        browser_name: str | None,
        cookie_file_path: Path | None,
        platform: str,
    ) -> None:
        ydl_opts, ffmpeg_available = self.make_ydl_options(
            platform,
            browser_name,
            cookie_file_path,
        )

        if ffmpeg_available:
            self.emit_event(
                {
                    "type": "log",
                    "message": "ffmpeg を検出しました。結合が必要な形式も選択できます。",
                }
            )
        else:
            self.emit_event(
                {
                    "type": "log",
                    "message": "ffmpeg が見つからないため、音声入りの単体形式のみ一覧に出します。",
                }
            )

        if cookie_file_path:
            self.emit_event(
                {
                    "type": "log",
                    "message": "Cookieファイルを使用してアクセスします。",
                }
            )
        elif browser_name:
            self.emit_event(
                {
                    "type": "log",
                    "message": f"{browser_name} のCookieを使用してアクセスします。",
                }
            )

        if platform == "YouTube" and "js_runtimes" in ydl_opts:
            runtime_name = next(iter(ydl_opts["js_runtimes"]))  # type: ignore[index]
            self.emit_event(
                {
                    "type": "log",
                    "message": f"YouTube 抽出用に {runtime_name} を使用します。",
                }
            )

        try:
            with yt_dlp.YoutubeDL(ydl_opts) as ydl:
                info = ydl.extract_info(url, download=False)

            title = str(info.get("title") or info.get("id") or "video")
            choices = build_format_choices(info, platform, ffmpeg_available)
            if not choices:
                raise RuntimeError("選択可能な動画形式を取得できませんでした。")

            self.emit_event(
                {
                    "type": "formats_loaded",
                    "title": title,
                    "platform": platform,
                    "url": url,
                    "output_dir": output_dir,
                    "browser_name": browser_name,
                    "cookie_file_path": cookie_file_path,
                    "choices": choices,
                }
            )
        except Exception as exc:
            self.emit_event(
                {
                    "type": "done",
                    "success": False,
                    "message": format_download_error(exc),
                }
            )

    def show_format_dialog(
        self,
        title: str,
        platform: str,
        url: str,
        output_dir: Path,
        browser_name: str | None,
        cookie_file_path: Path | None,
        choices: list[FormatChoice],
    ) -> None:
        self.status_var.set("利用可能な画質を選択してください。")
        self.append_log(f"タイトル: {title}")
        self.append_log(f"取得形式数: {len(choices)}")

        def on_confirm(choice: FormatChoice) -> None:
            self.format_dialog = None
            self.start_progress_tracking(choice)
            self.append_log(f"選択形式: {describe_selected_format(choice)}")
            self.status_var.set(f"{title} をダウンロード中...")
            worker = threading.Thread(
                target=self.download_selected_format,
                args=(url, output_dir, browser_name, cookie_file_path, platform, choice),
                daemon=True,
            )
            worker.start()

        def on_cancel() -> None:
            self.format_dialog = None
            self.is_busy = False
            self.download_button.configure(state="normal")
            self.reset_progress_tracking()
            self.status_var.set("形式選択をキャンセルしました。")
            self.append_log("形式選択をキャンセルしました。")

        self.format_dialog = FormatSelectionDialog(
            self,
            title_text=title,
            platform=platform,
            choices=choices,
            on_confirm=on_confirm,
            on_cancel=on_cancel,
        )

    def download_selected_format(
        self,
        url: str,
        output_dir: Path,
        browser_name: str | None,
        cookie_file_path: Path | None,
        platform: str,
        choice: FormatChoice,
    ) -> None:
        ydl_opts, _ffmpeg_available = self.make_ydl_options(
            platform=platform,
            browser_name=browser_name,
            cookie_file_path=cookie_file_path,
            output_dir=output_dir,
            format_selector=choice.selector,
            include_progress=True,
        )

        try:
            with FastYoutubeDL(ydl_opts) as ydl:
                info = ydl.extract_info(url, download=False)
                if not isinstance(info, dict):
                    raise RuntimeError("ダウンロード情報を取得できませんでした。")

                initial_progress = self.progress_tracker.prepare(info)
                total_label = (
                    f"{'約 ' if initial_progress.total_is_estimate else ''}"
                    f"{human_readable_size(initial_progress.total_bytes)}"
                    if initial_progress.total_bytes is not None
                    else "確認中"
                )
                self.emit_event(
                    {
                        "type": "progress",
                        "progress": initial_progress.progress,
                        "message": f"ダウンロードを開始します... 合計 {total_label}",
                    }
                )
                ydl.process_info(info)

            self.emit_event(
                {
                    "type": "done",
                    "success": True,
                    "message": "ダウンロードが完了しました。",
                }
            )
        except Exception as exc:
            self.emit_event(
                {
                    "type": "done",
                    "success": False,
                    "message": format_download_error(exc),
                }
            )

    def on_progress(self, data: dict[str, object]) -> None:
        status = data.get("status")
        if status in {"downloading", "finished", "error"}:
            snapshot = self.progress_tracker.update(data)
            if not snapshot.should_emit:
                return

            if snapshot.total_bytes is None:
                amount_text = (
                    f"{human_readable_size(snapshot.downloaded_bytes)} / 合計サイズ確認中"
                )
                percent_text = "計測中"
            else:
                approximate = "約 " if snapshot.total_is_estimate else ""
                amount_text = (
                    f"{human_readable_size(snapshot.downloaded_bytes)} / "
                    f"{approximate}{human_readable_size(snapshot.total_bytes)}"
                )
                percent_value = (snapshot.progress or 0.0) * 100
                percent_text = f"{approximate}{percent_value:5.1f}%"

            if snapshot.all_finished:
                status_text = f"ダウンロード完了。{amount_text}  後処理を実行しています..."
            elif status == "error":
                status_text = f"ダウンロードを再試行しています... {amount_text}"
            else:
                speed_text = (
                    f"{human_readable_size(snapshot.speed)}/s"
                    if snapshot.speed > 0
                    else "-- B/s"
                )
                status_text = (
                    f"ダウンロード中... {percent_text}  {amount_text}  "
                    f"{speed_text}  ETA {format_eta(snapshot.eta)}"
                )

            self.emit_event(
                {
                    "type": "progress",
                    "progress": snapshot.progress,
                    "message": status_text,
                }
            )

        if status == "finished":
            filename = str(data.get("filename") or "")
            if filename:
                self.emit_event({"type": "log", "message": f"保存ファイル: {filename}"})

    def emit_event(self, event: dict[str, object]) -> None:
        self.event_queue.put(event)

    def process_events(self) -> None:
        while True:
            try:
                event = self.event_queue.get_nowait()
            except queue.Empty:
                break

            event_type = event.get("type")
            if event_type == "log":
                self.append_log(str(event.get("message", "")))
            elif event_type == "status":
                self.status_var.set(str(event.get("message", "")))
            elif event_type == "progress":
                progress = event.get("progress")
                if isinstance(progress, (int, float)):
                    self.set_progress(float(progress))
                else:
                    self.set_indeterminate_progress()
                self.status_var.set(str(event.get("message", "")))
            elif event_type == "formats_loaded":
                choices = event.get("choices")
                cookie_file_path = event.get("cookie_file_path")
                self.show_format_dialog(
                    title=str(event.get("title", "")),
                    platform=str(event.get("platform", "")),
                    url=str(event.get("url", "")),
                    output_dir=Path(str(event.get("output_dir", ""))),
                    browser_name=event.get("browser_name") if isinstance(event.get("browser_name"), str) or event.get("browser_name") is None else None,
                    cookie_file_path=cookie_file_path if isinstance(cookie_file_path, Path) else None,
                    choices=choices if isinstance(choices, list) else [],
                )
            elif event_type == "done":
                self.is_busy = False
                self.download_button.configure(state="normal")
                success = bool(event.get("success"))
                message = str(event.get("message", ""))
                self.status_var.set(message)
                if success:
                    self.set_progress(1.0, immediate=True)
                    self.append_log(message)
                    messagebox.showinfo("完了", message)
                else:
                    self.reset_progress_tracking()
                    self.append_log(f"失敗: {message}")
                    messagebox.showerror("ダウンロードに失敗しました", message)

        self.after(100, self.process_events)

    def append_log(self, _message: str) -> None:
        return


if __name__ == "__main__":
    app = VideoDownloaderApp()
    app.mainloop()
