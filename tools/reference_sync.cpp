// The commit policy is inserted verbatim from Node_update.cpp by the generator.
#include <mmx/BlockHeader.hxx>
#include <mmx/ChainParams.hxx>
#include <vnx/vnx.h>
#include <fstream>
struct Fork { std::shared_ptr<mmx::BlockHeader> block; };
int main(int argc,char**argv) {
    if(argc!=3)return 1;
    std::ifstream in(argv[1]);std::ofstream out(argv[2]);std::string text;
    while(std::getline(in,text)) {
        auto row=vnx::from_string<vnx::Object>(text);
        auto read=[](vnx::Object v){auto b=mmx::BlockHeader::create();b->height=v["height"].to<uint32_t>();b->vdf_height=v["vdf_height"].to<uint32_t>();b->proof.resize(v["proofs"].to<size_t>());return b;};
        auto root=read(row["root"].to<vnx::Object>());
        std::vector<std::shared_ptr<Fork>> fork_line;
        for(auto v:row["line"].to<std::vector<vnx::Object>>()) {auto f=std::make_shared<Fork>();f->block=read(v);fork_line.push_back(f);}
        auto peak=fork_line.back()->block;
        auto params=mmx::ChainParams::create();params->avg_proof_count=row["average"].to<uint32_t>();params->commit_delay=row["delay"].to<uint32_t>();
        const auto commit_threshold=row["threshold"].to<uint32_t>(),max_future_sync=row["future"].to<uint32_t>(),sync_pos=row["sync_pos"].to<uint32_t>();
        const bool is_synced=row["synced"].to<bool>();
        const auto sync_pending=row["pending"].to<std::set<uint32_t>>();
        uint32_t fork_weight=0;
        std::vector<uint32_t> heights;
        auto commit=[&](std::shared_ptr<const mmx::BlockHeader>b){heights.push_back(b->height);};
        // MMX_ROOT_POLICY
        row["weight"]=fork_weight;row["heights"]=heights;
        out<<vnx::to_string(row)<<'\n';
    }
}
