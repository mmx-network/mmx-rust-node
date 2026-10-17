// find_best_fork is inserted verbatim from native Node.cpp by the generator.
#include <mmx/BlockHeader.hxx>
#include <vnx/vnx.h>
#include <fstream>
namespace mmx {
struct ForkReference {
    struct fork_t {
        bool is_invalid=false, is_proof_verified=false, is_all_proof_verified=false;
        uint32_t votes=0, total_votes=0;
        std::weak_ptr<fork_t> prev;
        std::shared_ptr<const BlockHeader> block, root;
    };
    std::shared_ptr<BlockHeader> root;
    std::map<hash_t,std::shared_ptr<BlockHeader>> headers;
    std::multimap<uint32_t,std::shared_ptr<fork_t>> fork_index;
    std::set<uint32_t> sync_pending;
    auto get_root()const{return root;}
    std::shared_ptr<const BlockHeader> find_prev(std::shared_ptr<const BlockHeader>b)const{auto i=headers.find(b->prev);return i==headers.end()?nullptr:i->second;}
    std::shared_ptr<fork_t> find_best_fork()const;
};
// MMX_FORK_CHOICE
}
int main(int argc,char**argv) {
    if(argc!=3)return 1;
    std::ifstream in(argv[1]);std::ofstream out(argv[2]);std::string line;
    while(std::getline(in,line)) {
        auto row=vnx::from_string<vnx::Object>(line);mmx::ForkReference r;
        r.root=mmx::BlockHeader::create();r.root->height=row["root_height"].to<uint32_t>();r.root->hash=row["root"].to<mmx::hash_t>();
        if(!row["sync_pending"].is_null())r.sync_pending.insert(row["sync_pending"].to<uint32_t>());
        std::map<mmx::hash_t,std::shared_ptr<mmx::ForkReference::fork_t>> forks;
        std::vector<std::shared_ptr<mmx::ForkReference::fork_t>> order;
        for(const auto& v:row["candidates"].to<std::vector<vnx::Object>>()) {
            auto b=mmx::BlockHeader::create();b->hash=v["hash"].to<mmx::hash_t>();b->prev=v["prev"].to<mmx::hash_t>();b->height=v["height"].to<uint32_t>();b->total_weight=v["total_weight"].to<mmx::uint128>();
            auto f=std::make_shared<mmx::ForkReference::fork_t>();f->block=b;f->votes=v["votes"].to<uint32_t>();f->is_invalid=v["invalid"].to<bool>();f->is_proof_verified=v["proof_verified"].to<bool>();
            if(!v["anchor"].is_null()){auto a=mmx::BlockHeader::create();auto x=v["anchor"].to<vnx::Object>();a->hash=x["hash"].to<mmx::hash_t>();a->total_weight=x["total_weight"].to<mmx::uint128>();r.headers[a->hash]=a;}
            forks[b->hash]=f;order.push_back(f);r.fork_index.emplace(b->height,f);
        }
        for(auto f:order){auto p=forks.find(f->block->prev);if(p!=forks.end())f->prev=p->second;}
        auto best=r.find_best_fork();row["best"]=best?best->block->hash.to_string():std::string();
        std::vector<vnx::Object> states;for(auto f:order){if(f->block->height<=r.root->height)continue;vnx::Object s;s["hash"]=f->block->hash.to_string();s["total_votes"]=f->total_votes;s["invalid"]=f->is_invalid;s["all_proofs_verified"]=f->is_all_proof_verified;if(f->root){s["anchor"]=f->root->hash.to_string();s["anchor_weight"]=f->root->total_weight.to_string();}states.push_back(s);}row["states"]=states;
        out<<vnx::to_string(row)<<'\n';
    }
}
