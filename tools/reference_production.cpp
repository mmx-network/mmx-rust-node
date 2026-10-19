// Find a second, stronger native proof for the synthetic chain's challenge.
// All test secrets are public scalars. Never opens node or wallet storage.
#include <mmx/Block.hxx>
#include <mmx/ProofOfSpaceOG.hxx>
#include <mmx/ProofOfSpaceNFT.hxx>
#include <mmx/ProofResponse.hxx>
#include <mmx/hash_512_t.hpp>
#include <mmx/pos/verify.h>
#include <mmx/secp256k1.hpp>
#include <mmx/utils.h>
#include <vnx/vnx.h>
#include <fstream>
#include <numeric>

namespace mmx {
struct ProductionReference {
    std::shared_ptr<ChainParams> params;
    void verify_proof(std::shared_ptr<const ProofOfSpace>, const hash_t&, uint64_t, const uint32_t&) const;
    template<class T> void verify_proof_impl(std::shared_ptr<const T>, const hash_t&, uint64_t, const uint32_t&) const;
};
// MMX_PROOF_METHODS
namespace pos {
void compute_f1(std::vector<uint32_t>*, std::vector<uint32_t>&,
    std::vector<std::array<uint32_t, N_META>>&, std::mutex&, uint32_t, const hash_t&, int, int);
// MMX_CANDIDATE
}
}
using namespace mmx;
template<class T> std::shared_ptr<T> decode(const std::string& text) {
    auto bytes = vnx::from_hex_string(text);
    vnx::VectorInputStream stream(&bytes); vnx::TypeInput in(&stream);
    std::shared_ptr<T> value; vnx::read(in, value); return value;
}
std::string wire(std::shared_ptr<const vnx::Value> value) {
    std::vector<uint8_t> bytes; vnx::VectorOutputStream stream(&bytes);
    vnx::TypeOutput out(&stream); vnx::write(out, value); out.flush();
    return vnx::to_hex_string(bytes.data(), bytes.size());
}
int main(int argc, char** argv) {
    if(argc != 3) return 1;
    secp256k1_init(); pos::set_remote_compute(false);
    ProductionReference reference;
    std::shared_ptr<Block> first;
    std::ifstream input(argv[1]); std::string line;
    while(std::getline(input, line)) {
        auto row = vnx::from_string<vnx::Object>(line);
        if(row["name"].to<std::string>() == "synthetic") reference.params = decode<ChainParams>(row["params"].to<std::string>());
        if(row["kind"].to<std::string>() == "chain") first = decode<Block>(row["steps"].to<std::vector<vnx::Object>>()[0]["block"].to<std::string>());
    }
    if(!reference.params || !first) throw std::logic_error("missing synthetic chain");
    skey_t secret; secret[31] = 2;
    for(uint64_t seed = 0; seed < 10000; ++seed) {
        auto proof = ProofOfSpaceOG::create(); proof->ksize = 8;
        proof->seed.from_uint(seed); proof->farmer_key = pubkey_t(secret);
        proof->plot_id = hash_t(std::string("MMX/PLOTID/OG") + bytes_t<1>(&proof->ksize, 1) + proof->seed + proof->farmer_key);
        proof->challenge = first->proof[0]->challenge; proof->difficulty = 1;
        const auto y = get_plot_challenge(proof->challenge, proof->plot_id).to_uint<uint32_t>() & 255;
        std::vector<uint32_t> xs, ys, ordered;
        std::vector<std::array<uint32_t, pos::N_META>> meta; std::mutex mutex;
        for(unsigned x = 0; x < 256; ++x) pos::compute_f1(&xs, ys, meta, mutex, x, proof->plot_id, 8, 0);
        std::vector<std::pair<uint32_t, bytes_t<pos::META_BYTES_OUT>>> entries;
        try { entries = pos::candidate_full(xs, ys, meta, &ordered, proof->plot_id, 8); }
        catch(const std::exception&) { continue; }
        for(size_t i = 0; i < entries.size(); ++i) {
            if(entries[i].first != y) continue;
            proof->proof_xs = {ordered.begin() + i * 256, ordered.begin() + (i + 1) * 256};
            proof->score = get_proof_score(proof->calc_proof_hash());
            if(!(proof->calc_proof_hash() < first->proof_hash)) continue;
            try { reference.verify_proof(proof, proof->challenge, 1, first->vdf_height); }
            catch(const std::exception&) { continue; }
            auto response = ProofResponse::create(); response->proof = proof;
            response->vdf_height = first->vdf_height; response->farmer_addr = vnx::Hash64(uint64_t(12345));
            response->harvester = "private test farm"; response->hash = response->calc_hash();
            response->farmer_sig = signature_t::sign(secret, response->hash);
            response->content_hash = response->calc_content_hash(); response->validate();
            vnx::Object row; row["response"] = wire(response);
            row["proof_hash"] = proof->calc_proof_hash().to_string(); row["score"] = proof->score;
            row["weaker_hash"] = first->proof_hash.to_string(); row["verified"] = true;
            std::ofstream output(argv[2]); output << vnx::to_string(row) << '\n';
            std::cout << "Native stronger proof verified, seed " << seed << '\n'; return 0;
        }
    }
    throw std::logic_error("could not find a stronger native proof");
}
