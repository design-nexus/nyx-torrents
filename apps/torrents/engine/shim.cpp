// A thin, value-only wrapper around libtorrent's session for the Rust side.
// Everything is keyed by the torrent's id: the hex of info_hashes().get_best().

#include "nyx-torrents/engine/shim.h"
#include "nyx-torrents/src/engine/ffi.rs.h"

#include <libtorrent/add_torrent_params.hpp>
#include <libtorrent/alert_types.hpp>
#include <libtorrent/announce_entry.hpp>
#include <libtorrent/create_torrent.hpp>
#include <libtorrent/load_torrent.hpp>
#include <libtorrent/magnet_uri.hpp>
#include <libtorrent/peer_info.hpp>
#include <libtorrent/read_resume_data.hpp>
#include <libtorrent/session_params.hpp>
#include <libtorrent/session_stats.hpp>
#include <libtorrent/settings_pack.hpp>
#include <libtorrent/torrent_info.hpp>
#include <libtorrent/version.hpp>
#include <libtorrent/write_resume_data.hpp>

#include <algorithm>
#include <chrono>
#include <ctime>
#include <stdexcept>

namespace nt {

namespace {

std::string hex(char const* data, std::size_t len) {
    static char const digits[] = "0123456789abcdef";
    std::string out;
    out.reserve(len * 2);
    for (std::size_t i = 0; i < len; ++i) {
        auto c = static_cast<unsigned char>(data[i]);
        out.push_back(digits[c >> 4]);
        out.push_back(digits[c & 15]);
    }
    return out;
}

template <class Digest>
std::string hex(Digest const& d) {
    return hex(d.data(), static_cast<std::size_t>(d.size()));
}

std::string id_of(lt::info_hash_t const& ih) {
    return hex(ih.get_best());
}

lt::span<char const> as_span(rust::Slice<const std::uint8_t> s) {
    return {reinterpret_cast<char const*>(s.data()), static_cast<std::ptrdiff_t>(s.size())};
}

rust::Vec<std::uint8_t> to_vec(std::vector<char> const& buf) {
    rust::Vec<std::uint8_t> out;
    out.reserve(buf.size());
    for (char c : buf) out.push_back(static_cast<std::uint8_t>(c));
    return out;
}

std::int64_t secs_until(lt::time_point32 t) {
    auto now = lt::clock_type::now();
    if (t <= now) return 0;
    return std::chrono::duration_cast<std::chrono::seconds>(t - now).count();
}

lt::settings_pack make_pack(rust::Vec<Kv> const& settings, std::string& errors) {
    lt::settings_pack pack;
    for (auto const& kv : settings) {
        std::string key(kv.key);
        std::string value(kv.value);
        int idx = lt::setting_by_name(key);
        if (idx < 0) {
            errors += "unknown setting " + key + "; ";
            continue;
        }
        try {
            switch (idx & lt::settings_pack::type_mask) {
            case lt::settings_pack::string_type_base:
                pack.set_str(idx, value);
                break;
            case lt::settings_pack::int_type_base:
                pack.set_int(idx, std::stoi(value));
                break;
            case lt::settings_pack::bool_type_base:
                pack.set_bool(idx, value == "true" || value == "1");
                break;
            }
        } catch (std::exception const&) {
            errors += "bad value for " + key + "; ";
        }
    }
    pack.set_int(lt::settings_pack::alert_mask,
        static_cast<int>(static_cast<std::uint32_t>(lt::alert_category::error | lt::alert_category::status
            | lt::alert_category::storage | lt::alert_category::file_progress)));
    pack.set_int(lt::settings_pack::alert_queue_size, 10000);
    return pack;
}

FileEntry file_entry(lt::file_storage const& fs, lt::file_index_t i) {
    FileEntry f;
    f.path = fs.file_path(i);
    f.size = fs.file_size(i);
    f.done = 0;
    // 255 marks padding files, which the UI skips (indices must stay aligned).
    f.priority = fs.pad_file_at(i) ? 255 : 4;
    return f;
}

TorrentInfo info_from(lt::add_torrent_params const& atp) {
    TorrentInfo info;
    info.id = id_of(atp.ti ? atp.ti->info_hashes() : atp.info_hashes);
    if (atp.ti) {
        auto const& fs = atp.ti->files();
        info.name = atp.ti->name();
        info.size = fs.total_size();
        info.comment = atp.ti->comment();
        info.private_torrent = atp.ti->priv();
        info.piece_length = atp.ti->piece_length();
        info.num_pieces = atp.ti->num_pieces();
        for (auto i : fs.file_range()) info.files.push_back(file_entry(fs, i));
    } else {
        info.name = atp.name;
    }
    return info;
}

Status to_status(lt::torrent_status const& st) {
    Status s;
    s.id = id_of(st.info_hashes);
    s.name = st.name;
    s.save_path = st.save_path;
    s.state = static_cast<std::uint8_t>(st.state);
    s.paused = bool(st.flags & lt::torrent_flags::paused);
    s.auto_managed = bool(st.flags & lt::torrent_flags::auto_managed);
    s.sequential = bool(st.flags & lt::torrent_flags::sequential_download);
    s.super_seeding = bool(st.flags & lt::torrent_flags::super_seeding);
    s.has_metadata = st.has_metadata;
    s.moving = st.moving_storage;
    s.is_finished = st.is_finished;
    s.is_seeding = st.is_seeding;
    s.progress = st.progress;
    s.size = st.total;
    s.wanted = st.total_wanted;
    s.wanted_done = st.total_wanted_done;
    s.total_done = st.total_done;
    s.uploaded = st.all_time_upload;
    s.downloaded = st.all_time_download;
    s.dl_rate = st.download_payload_rate;
    s.ul_rate = st.upload_payload_rate;
    s.seeds = st.num_seeds;
    s.peers = st.num_peers - st.num_seeds;
    s.seeds_total = st.num_complete >= 0 ? st.num_complete : st.list_seeds;
    s.peers_total = st.num_incomplete >= 0 ? st.num_incomplete : st.list_peers - st.list_seeds;
    s.queue_position = static_cast<std::int32_t>(st.queue_position);
    s.availability = st.distributed_copies;
    if (st.errc) {
        s.error = st.errc.message();
    }
    s.added = st.added_time;
    s.completed = st.completed_time;
    {
        auto now = std::time(nullptr);
        auto last = std::max(st.last_download, st.last_upload);
        s.last_activity = last.time_since_epoch().count() == 0
            ? 0
            : now - std::chrono::duration_cast<std::chrono::seconds>(lt::clock_type::now() - last).count();
    }
    s.seeding_secs = st.seeding_duration.count();
    s.active_secs = st.active_duration.count();
    s.dl_limit = 0;
    s.ul_limit = 0;
    s.num_pieces = st.num_pieces;
    s.pieces_done = st.num_pieces > 0 ? static_cast<std::int32_t>(st.pieces.count()) : 0;
    s.piece_length = st.block_size;
    s.tracker = st.current_tracker;
    s.next_announce = std::chrono::duration_cast<std::chrono::seconds>(st.next_announce).count();
    return s;
}

// Give each wanted file's first and last piece top priority, so previews work early.
void apply_first_last(lt::torrent_handle const& h, bool on) {
    auto ti = h.torrent_file();
    if (!ti) return;
    auto const& fs = ti->files();
    auto file_prios = h.get_file_priorities();
    auto piece_prios = h.get_piece_priorities();
    for (auto i : fs.file_range()) {
        if (fs.pad_file_at(i) || fs.file_size(i) == 0) continue;
        auto fi = static_cast<int>(i);
        if (fi < static_cast<int>(file_prios.size()) && file_prios[fi] == lt::dont_download) continue;
        auto first = fs.map_file(i, 0, 0).piece;
        auto last = fs.map_file(i, std::max<std::int64_t>(fs.file_size(i) - 1, 0), 0).piece;
        auto prio = on ? lt::top_priority
                       : (fi < static_cast<int>(file_prios.size()) ? file_prios[fi] : lt::default_priority);
        for (auto p : {first, last}) {
            auto pi = static_cast<int>(p);
            if (pi >= 0 && pi < static_cast<int>(piece_prios.size())) piece_prios[pi] = prio;
        }
    }
    h.prioritize_pieces(piece_prios);
}

std::string tail_after_slash(std::string const& path) {
    auto pos = path.find('/');
    return pos == std::string::npos ? path : path.substr(pos + 1);
}

} // namespace

// ---------- Session ----------

Session::Session(rust::Vec<Kv> const& settings, rust::Slice<const std::uint8_t> state) {
    lt::session_params params;
    if (state.size() > 0) {
        try {
            params = lt::read_session_params(as_span(state), lt::session_handle::save_dht_state);
        } catch (std::exception const&) {
            params = lt::session_params();
        }
    }
    std::string errors;
    params.settings = make_pack(settings, errors);
    ses_ = std::make_unique<lt::session>(std::move(params));
    idx_dht_nodes_ = lt::find_metric_idx("dht.dht_nodes");
    idx_recv_payload_ = lt::find_metric_idx("net.recv_payload_bytes");
    idx_sent_payload_ = lt::find_metric_idx("net.sent_payload_bytes");
    last_stats_ = lt::clock_type::now();
}

rust::String Session::apply_settings(rust::Vec<Kv> const& settings) {
    std::string errors;
    ses_->apply_settings(make_pack(settings, errors));
    return rust::String(errors);
}

lt::torrent_handle Session::find(rust::Str id) const {
    auto it = handles_.find(std::string(id));
    return it == handles_.end() ? lt::torrent_handle() : it->second;
}

rust::String Session::add_torrent(AddParams const& p) {
    lt::add_torrent_params atp;
    lt::error_code ec;
    bool from_resume = false;
    if (!p.resume.empty()) {
        atp = lt::read_resume_data(as_span(rust::Slice<const std::uint8_t>(p.resume.data(), p.resume.size())), ec);
        if (ec) throw std::runtime_error("resume data: " + ec.message());
        from_resume = true;
    } else if (!p.magnet.empty()) {
        atp = lt::parse_magnet_uri(std::string(p.magnet), ec);
        if (ec) throw std::runtime_error("That isn't a valid magnet link (" + ec.message() + ")");
    } else {
        atp = lt::load_torrent_buffer(as_span(rust::Slice<const std::uint8_t>(p.torrent.data(), p.torrent.size())));
    }

    if (!from_resume) {
        atp.save_path = std::string(p.save_path);
        atp.flags &= ~(lt::torrent_flags::paused | lt::torrent_flags::auto_managed);
        if (p.paused) atp.flags |= lt::torrent_flags::paused;
        if (p.auto_managed) atp.flags |= lt::torrent_flags::auto_managed;
        if (p.sequential) atp.flags |= lt::torrent_flags::sequential_download;
        if (p.skip_check) atp.flags |= lt::torrent_flags::seed_mode;
        if (p.disable_pex) atp.flags |= lt::torrent_flags::disable_pex;
        atp.storage_mode = p.preallocate ? lt::storage_mode_allocate : lt::storage_mode_sparse;
        atp.download_limit = p.dl_limit > 0 ? p.dl_limit : -1;
        atp.upload_limit = p.ul_limit > 0 ? p.ul_limit : -1;
        for (auto const& t : p.trackers) {
            std::string url(t);
            if (std::find(atp.trackers.begin(), atp.trackers.end(), url) == atp.trackers.end()) {
                atp.trackers.push_back(url);
            }
        }
        if (!p.file_priorities.empty()) {
            atp.file_priorities.clear();
            for (auto prio : p.file_priorities) atp.file_priorities.push_back(lt::download_priority_t(prio));
        }
        std::string name(p.name);
        if (atp.ti) {
            auto const& fs = atp.ti->files();
            bool single = fs.num_files() == 1;
            for (auto i : fs.file_range()) {
                std::string path = fs.file_path(i);
                std::string renamed = path;
                if (p.content_layout == 1 && single) {
                    std::string stem = path;
                    auto dot = stem.rfind('.');
                    if (dot != std::string::npos && dot > 0) stem = stem.substr(0, dot);
                    renamed = (name.empty() ? stem : name) + "/" + path;
                } else if (p.content_layout == 2 && !single) {
                    renamed = tail_after_slash(path);
                } else if (!name.empty() && !single) {
                    renamed = name + "/" + tail_after_slash(path);
                } else if (!name.empty() && single && p.content_layout != 1) {
                    renamed = name;
                }
                if (renamed != path) atp.renamed_files[i] = renamed;
            }
        } else if (!name.empty()) {
            atp.name = name;
        }
    }
    atp.flags |= lt::torrent_flags::duplicate_is_error;

    auto h = ses_->add_torrent(std::move(atp), ec);
    if (ec) {
        if (ec == lt::errors::duplicate_torrent) throw std::runtime_error("This torrent is already in the list");
        throw std::runtime_error(ec.message());
    }
    auto id = id_of(h.info_hashes());
    handles_[id] = h;
    status_[id] = h.status();
    if (p.first_last && h.torrent_file()) apply_first_last(h, true);
    return rust::String(id);
}

rust::String Session::fetch_metadata(rust::Str magnet, rust::Str save_path) {
    lt::error_code ec;
    auto atp = lt::parse_magnet_uri(std::string(magnet), ec);
    if (ec) throw std::runtime_error("That isn't a valid magnet link (" + ec.message() + ")");
    auto id = id_of(atp.info_hashes);
    if (handles_.count(id) && !fetching_.count(id)) throw std::runtime_error("This torrent is already in the list");
    if (fetching_.count(id)) return rust::String(id);
    atp.save_path = std::string(save_path);
    atp.flags &= ~(lt::torrent_flags::paused | lt::torrent_flags::auto_managed);
    atp.flags |= lt::torrent_flags::upload_mode;
    // Download nothing, whatever the torrent turns out to hold.
    atp.file_priorities.assign(100000, lt::dont_download);
    auto h = ses_->add_torrent(std::move(atp), ec);
    if (ec) throw std::runtime_error(ec.message());
    handles_[id] = h;
    fetching_.insert(id);
    return rust::String(id);
}

void Session::cancel_fetch(rust::Str id) {
    std::string key(id);
    if (!fetching_.count(key)) return;
    if (auto h = find(id); h.is_valid()) ses_->remove_torrent(h);
    fetching_.erase(key);
    handles_.erase(key);
}

void Session::remove(rust::Str id, bool with_files) {
    auto h = find(id);
    if (!h.is_valid()) return;
    ses_->remove_torrent(h, with_files ? lt::session_handle::delete_files : lt::remove_flags_t{});
    handles_.erase(std::string(id));
    status_.erase(std::string(id));
}

void Session::pause(rust::Str id) {
    if (auto h = find(id); h.is_valid()) {
        h.unset_flags(lt::torrent_flags::auto_managed);
        h.pause(lt::torrent_handle::graceful_pause);
        h.save_resume_data(lt::torrent_handle::only_if_modified);
    }
}

void Session::resume(rust::Str id, bool force, bool auto_managed) {
    if (auto h = find(id); h.is_valid()) {
        if (force || !auto_managed) {
            h.unset_flags(lt::torrent_flags::auto_managed);
        } else {
            h.set_flags(lt::torrent_flags::auto_managed);
        }
        h.resume();
        h.save_resume_data(lt::torrent_handle::only_if_modified);
    }
}

void Session::recheck(rust::Str id) {
    if (auto h = find(id); h.is_valid()) h.force_recheck();
}

void Session::reannounce(rust::Str id) {
    if (auto h = find(id); h.is_valid()) {
        h.force_reannounce();
        h.force_dht_announce();
    }
}

void Session::queue_move(rust::Str id, std::uint8_t how) {
    auto h = find(id);
    if (!h.is_valid()) return;
    switch (how) {
    case 0: h.queue_position_up(); break;
    case 1: h.queue_position_down(); break;
    case 2: h.queue_position_top(); break;
    default: h.queue_position_bottom(); break;
    }
}

void Session::set_file_priorities(rust::Str id, rust::Slice<const std::uint8_t> priorities) {
    auto h = find(id);
    if (!h.is_valid()) return;
    std::vector<lt::download_priority_t> prios;
    for (auto p : priorities) prios.push_back(lt::download_priority_t(p));
    h.prioritize_files(prios);
    h.save_resume_data(lt::torrent_handle::only_if_modified);
}

void Session::set_limits(rust::Str id, std::int32_t dl, std::int32_t ul) {
    if (auto h = find(id); h.is_valid()) {
        h.set_download_limit(dl > 0 ? dl : -1);
        h.set_upload_limit(ul > 0 ? ul : -1);
        h.save_resume_data(lt::torrent_handle::only_if_modified);
    }
}

void Session::set_sequential(rust::Str id, bool on) {
    if (auto h = find(id); h.is_valid()) {
        if (on) h.set_flags(lt::torrent_flags::sequential_download);
        else h.unset_flags(lt::torrent_flags::sequential_download);
    }
}

void Session::set_first_last(rust::Str id, bool on) {
    if (auto h = find(id); h.is_valid()) apply_first_last(h, on);
}

void Session::set_super_seeding(rust::Str id, bool on) {
    if (auto h = find(id); h.is_valid()) {
        if (on) h.set_flags(lt::torrent_flags::super_seeding);
        else h.unset_flags(lt::torrent_flags::super_seeding);
    }
}

void Session::move_storage(rust::Str id, rust::Str path) {
    if (auto h = find(id); h.is_valid()) h.move_storage(std::string(path), lt::move_flags_t::dont_replace);
}

void Session::rename(rust::Str id, rust::Str name) {
    auto h = find(id);
    if (!h.is_valid()) return;
    auto ti = h.torrent_file();
    if (!ti) return;
    auto const& fs = ti->files();
    std::string n(name);
    if (fs.num_files() == 1) {
        h.rename_file(lt::file_index_t(0), n);
        return;
    }
    for (auto i : fs.file_range()) h.rename_file(i, n + "/" + tail_after_slash(fs.file_path(i)));
}

void Session::add_tracker(rust::Str id, rust::Str url, std::int32_t tier) {
    if (auto h = find(id); h.is_valid()) {
        lt::announce_entry e(std::string{url});
        e.tier = static_cast<std::uint8_t>(std::max(0, tier));
        h.add_tracker(e);
        h.save_resume_data(lt::torrent_handle::only_if_modified);
    }
}

void Session::remove_tracker(rust::Str id, rust::Str url) {
    auto h = find(id);
    if (!h.is_valid()) return;
    auto trackers = h.trackers();
    std::string u(url);
    trackers.erase(std::remove_if(trackers.begin(), trackers.end(), [&](auto const& t) { return t.url == u; }),
        trackers.end());
    h.replace_trackers(trackers);
    h.save_resume_data(lt::torrent_handle::only_if_modified);
}

void Session::save_resume(rust::Str id) {
    if (auto h = find(id); h.is_valid() && !fetching_.count(std::string(id))) {
        h.save_resume_data(lt::torrent_handle::save_info_dict);
    }
}

std::int32_t Session::save_all_resume(bool all) {
    std::int32_t n = 0;
    for (auto const& [id, h] : handles_) {
        if (fetching_.count(id) || !h.is_valid()) continue;
        if (!all && !h.need_save_resume_data()) continue;
        h.save_resume_data(lt::torrent_handle::save_info_dict);
        ++n;
    }
    return n;
}

void Session::post_updates() {
    ses_->post_torrent_updates();
    ses_->post_session_stats();
}

rust::Vec<Status> Session::statuses() {
    rust::Vec<Status> out;
    for (auto const& [id, st] : status_) {
        if (fetching_.count(id)) continue;
        auto s = to_status(st);
        s.id = id;
        if (auto h = handles_.find(id); h != handles_.end() && h->second.is_valid()) {
            s.dl_limit = h->second.download_limit();
            s.ul_limit = h->second.upload_limit();
        }
        out.push_back(std::move(s));
    }
    return out;
}

Details Session::details(rust::Str id, bool with_peers) {
    Details d;
    d.id = std::string(id);
    auto h = find(id);
    if (!h.is_valid()) return d;
    auto st = h.status(lt::torrent_handle::query_pieces);
    auto ti = h.torrent_file();
    auto ih = h.info_hashes();
    if (ih.has_v1()) d.hash_v1 = hex(ih.v1);
    if (ih.has_v2()) d.hash_v2 = hex(ih.v2);
    try {
        d.magnet = lt::make_magnet_uri(h);
    } catch (std::exception const&) {
    }

    if (ti) {
        auto const& fs = ti->files();
        d.comment = ti->comment();
        d.creator = ti->creator();
        d.created = static_cast<std::int64_t>(ti->creation_date());
        d.private_torrent = ti->priv();
        std::vector<std::int64_t> progress;
        h.file_progress(progress, lt::torrent_handle::piece_granularity);
        auto prios = h.get_file_priorities();
        for (auto i : fs.file_range()) {
            auto f = file_entry(fs, i);
            auto fi = static_cast<std::size_t>(static_cast<int>(i));
            if (fi < progress.size()) f.done = progress[fi];
            if (f.priority != 255 && fi < prios.size()) f.priority = static_cast<std::uint8_t>(prios[fi]);
            d.files.push_back(std::move(f));
        }
        int n = st.pieces.size();
        for (int i = 0; i < n; ++i) d.pieces.push_back(st.pieces[lt::piece_index_t(i)] ? 2 : 0);
        std::vector<lt::partial_piece_info> queue;
        h.get_download_queue(queue);
        for (auto const& q : queue) {
            auto pi = static_cast<int>(q.piece_index);
            if (pi >= 0 && pi < n && d.pieces[pi] == 0) d.pieces[pi] = 1;
        }
    }

    for (auto const& t : h.trackers()) {
        Tracker tr;
        tr.url = t.url;
        tr.tier = t.tier;
        tr.seeds = -1;
        tr.peers = -1;
        tr.downloaded = -1;
        bool any_working = false, any_updating = false, any_error = false, any_sent = false;
        std::int64_t next = -1;
        std::string message;
        for (auto const& ep : t.endpoints) {
            for (auto const& a : ep.info_hashes) {
                if (a.updating) any_updating = true;
                if (a.start_sent || a.fails > 0 || a.scrape_complete >= 0) any_sent = true;
                if (a.fails > 0) {
                    any_error = true;
                    if (message.empty()) message = a.last_error ? a.last_error.message() : a.message;
                } else if (a.start_sent) {
                    any_working = true;
                }
                if (message.empty() && !a.message.empty()) message = a.message;
                tr.seeds = std::max(tr.seeds, a.scrape_complete);
                tr.peers = std::max(tr.peers, a.scrape_incomplete);
                tr.downloaded = std::max(tr.downloaded, a.scrape_downloaded);
                auto s = secs_until(a.next_announce);
                if (a.next_announce != (lt::time_point32::min)() && (next < 0 || s < next)) next = s;
            }
        }
        tr.status = any_updating ? 2 : any_working ? 1 : any_error ? 3 : any_sent ? 1 : 0;
        tr.message = message;
        tr.next_announce = next;
        d.trackers.push_back(std::move(tr));
    }

    if (with_peers) {
        std::vector<lt::peer_info> peers;
        h.get_peer_info(peers);
        for (auto const& p : peers) {
            Peer out;
            auto addr = p.ip.address();
            out.address = addr.is_v6() ? "[" + addr.to_string() + "]:" + std::to_string(p.ip.port())
                                       : addr.to_string() + ":" + std::to_string(p.ip.port());
            out.client = p.client;
            out.progress = p.progress;
            out.dl_rate = p.payload_down_speed;
            out.ul_rate = p.payload_up_speed;
            out.downloaded = p.total_download;
            out.uploaded = p.total_upload;
            std::string f;
            if (p.flags & lt::peer_info::interesting) f += (p.flags & lt::peer_info::choked) ? "d" : "D";
            if (p.flags & lt::peer_info::remote_interested) f += (p.flags & lt::peer_info::remote_choked) ? "u" : "U";
            if (p.flags & lt::peer_info::optimistic_unchoke) f += "O";
            if (p.flags & lt::peer_info::snubbed) f += "S";
            if (p.source & lt::peer_info::incoming) f += "I";
            if (p.flags & (lt::peer_info::rc4_encrypted | lt::peer_info::plaintext_encrypted)) f += "E";
            if (p.source & lt::peer_info::pex) f += "X";
            if (p.source & lt::peer_info::dht) f += "H";
            if (p.source & lt::peer_info::lsd) f += "L";
            if (p.flags & lt::peer_info::utp_socket) f += "P";
            out.flags = f;
            std::string conn = (p.flags & lt::peer_info::utp_socket) ? "μTP" : "BT";
            if (p.flags & lt::peer_info::ssl_socket) conn += " (SSL)";
            out.connection = conn;
            d.peers.push_back(std::move(out));
        }
    }
    return d;
}

rust::Vec<Event> Session::poll() {
    rust::Vec<Event> events;
    auto push = [&](std::uint8_t kind, std::string const& id, std::string const& text) {
        Event e;
        e.kind = kind;
        e.id = id;
        e.text = text;
        events.push_back(std::move(e));
    };
    std::vector<lt::alert*> alerts;
    ses_->pop_alerts(&alerts);
    for (auto* a : alerts) {
        if (auto* su = lt::alert_cast<lt::state_update_alert>(a)) {
            for (auto const& st : su->status) {
                auto id = id_of(st.info_hashes);
                if (handles_.count(id)) status_[id] = st;
            }
        } else if (auto* mr = lt::alert_cast<lt::metadata_received_alert>(a)) {
            auto id = id_of(mr->handle.info_hashes());
            if (fetching_.count(id)) {
                auto ti = mr->handle.torrent_file();
                if (ti) {
                    lt::add_torrent_params atp;
                    atp.ti = std::make_shared<lt::torrent_info>(*ti);
                    for (auto const& t : mr->handle.trackers()) atp.trackers.push_back(t.url);
                    Event e;
                    e.kind = 3;
                    e.id = id;
                    e.text = ti->name();
                    try {
                        e.data = to_vec(lt::write_torrent_file_buf(atp, lt::write_flags::allow_missing_piece_layer));
                    } catch (std::exception const& ex) {
                        e.kind = 2;
                        e.text = ex.what();
                    }
                    events.push_back(std::move(e));
                }
                ses_->remove_torrent(mr->handle);
                fetching_.erase(id);
                handles_.erase(id);
            } else {
                push(4, id, "");
                mr->handle.save_resume_data(lt::torrent_handle::save_info_dict);
            }
        } else if (auto* tf = lt::alert_cast<lt::torrent_finished_alert>(a)) {
            auto id = id_of(tf->handle.info_hashes());
            if (!fetching_.count(id)) {
                push(5, id, tf->torrent_name());
                tf->handle.save_resume_data(lt::torrent_handle::save_info_dict);
            }
        } else if (auto* rd = lt::alert_cast<lt::save_resume_data_alert>(a)) {
            auto id = id_of(rd->params.info_hashes);
            if (!fetching_.count(id)) {
                Event e;
                e.kind = 7;
                e.id = id;
                e.data = to_vec(lt::write_resume_data_buf(rd->params));
                events.push_back(std::move(e));
            }
        } else if (auto* rf = lt::alert_cast<lt::save_resume_data_failed_alert>(a)) {
            push(12, id_of(rf->handle.info_hashes()), rf->error.message());
        } else if (auto* te = lt::alert_cast<lt::torrent_error_alert>(a)) {
            push(6, id_of(te->handle.info_hashes()), te->error.message() + (te->filename()[0] ? std::string(" (") + te->filename() + ")" : ""));
        } else if (auto* fe = lt::alert_cast<lt::file_error_alert>(a)) {
            push(6, id_of(fe->handle.info_hashes()), fe->error.message() + " (" + fe->filename() + ")");
        } else if (auto* tr = lt::alert_cast<lt::torrent_removed_alert>(a)) {
            push(8, id_of(tr->info_hashes), "");
        } else if (auto* td = lt::alert_cast<lt::torrent_deleted_alert>(a)) {
            push(9, id_of(td->info_hashes), "");
        } else if (auto* tdf = lt::alert_cast<lt::torrent_delete_failed_alert>(a)) {
            push(14, id_of(tdf->info_hashes), tdf->error.message());
        } else if (auto* sm = lt::alert_cast<lt::storage_moved_alert>(a)) {
            push(10, id_of(sm->handle.info_hashes()), sm->storage_path());
        } else if (auto* smf = lt::alert_cast<lt::storage_moved_failed_alert>(a)) {
            push(11, id_of(smf->handle.info_hashes()), smf->error.message());
        } else if (auto* lf = lt::alert_cast<lt::listen_failed_alert>(a)) {
            push(13, "", lf->message());
        } else if (auto* ss = lt::alert_cast<lt::session_stats_alert>(a)) {
            auto counters = ss->counters();
            auto now = lt::clock_type::now();
            double secs = std::chrono::duration<double>(now - last_stats_).count();
            auto recv = idx_recv_payload_ >= 0 ? counters[idx_recv_payload_] : 0;
            auto sent = idx_sent_payload_ >= 0 ? counters[idx_sent_payload_] : 0;
            if (secs > 0.2) {
                dl_rate_ = static_cast<std::int64_t>((recv - last_recv_) / secs);
                ul_rate_ = static_cast<std::int64_t>((sent - last_sent_) / secs);
                last_recv_ = recv;
                last_sent_ = sent;
                last_stats_ = now;
            }
            recv_total_ = recv;
            sent_total_ = sent;
            if (idx_dht_nodes_ >= 0) dht_nodes_ = counters[idx_dht_nodes_];
        }
    }
    return events;
}

Stats Session::stats() {
    Stats s;
    s.dl_rate = std::max<std::int64_t>(dl_rate_, 0);
    s.ul_rate = std::max<std::int64_t>(ul_rate_, 0);
    s.dht_nodes = dht_nodes_;
    s.downloaded = recv_total_;
    s.uploaded = sent_total_;
    s.listen_port = ses_->listen_port();
    s.has_incoming = ses_->is_listening();
    return s;
}

rust::Vec<std::uint8_t> Session::session_state() const {
    return to_vec(lt::write_session_params_buf(ses_->session_state(lt::session_handle::save_dht_state),
        lt::session_handle::save_dht_state));
}

rust::Vec<std::uint8_t> Session::torrent_file(rust::Str id) const {
    auto h = find(id);
    if (!h.is_valid()) return {};
    // With hashes, so a v2 torrent's piece layers make it into the file.
    auto ti = h.torrent_file_with_hashes();
    if (!ti) return {};
    lt::add_torrent_params atp;
    atp.ti = std::make_shared<lt::torrent_info>(*ti);
    for (auto const& t : h.trackers()) atp.trackers.push_back(t.url);
    try {
        return to_vec(lt::write_torrent_file_buf(atp, lt::write_flags::allow_missing_piece_layer));
    } catch (std::exception const&) {
        return {};
    }
}

void Session::pause_session() {
    ses_->pause();
}

// ---------- Free functions ----------

std::unique_ptr<Session> new_session(rust::Vec<Kv> const& settings, rust::Slice<const std::uint8_t> state) {
    return std::make_unique<Session>(settings, state);
}

TorrentInfo parse_torrent(rust::Slice<const std::uint8_t> data) {
    auto atp = lt::load_torrent_buffer(as_span(data));
    return info_from(atp);
}

TorrentInfo parse_magnet(rust::Str uri) {
    lt::error_code ec;
    auto atp = lt::parse_magnet_uri(std::string(uri), ec);
    if (ec) throw std::runtime_error("That isn't a valid magnet link (" + ec.message() + ")");
    return info_from(atp);
}

rust::Vec<std::uint8_t> create_torrent(rust::Str path, rust::Vec<rust::String> const& trackers, bool private_torrent) {
    std::string p(path);
    while (p.size() > 1 && p.back() == '/') p.pop_back();
#if LIBTORRENT_VERSION_NUM >= 20100
    auto files = lt::list_files(p);
    if (files.empty()) throw std::runtime_error("There's nothing to share at " + p);
    lt::create_torrent t(std::move(files));
#else
    // libtorrent 2.0 (NixOS, Debian): the file list goes through a file_storage.
    lt::file_storage files;
    lt::add_files(files, p);
    if (files.num_files() == 0) throw std::runtime_error("There's nothing to share at " + p);
    lt::create_torrent t(files);
#endif
    int tier = 0;
    for (auto const& tr : trackers) t.add_tracker(std::string(tr), tier++);
    t.set_priv(private_torrent);
    t.set_creator("Torrents " "0.1.0");
    auto slash = p.rfind('/');
    std::string parent = slash == std::string::npos ? "." : (slash == 0 ? "/" : p.substr(0, slash));
    lt::error_code ec;
    lt::set_piece_hashes(t, parent, ec);
    if (ec) throw std::runtime_error(ec.message());
    return to_vec(t.generate_buf());
}

} // namespace nt
