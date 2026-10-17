// Original ValidatorVote methods are linked. Native queue/history methods are
// copied verbatim by generate_vote_fixtures.py; only host services are stubbed.
#include <mmx/ValidatorVote.hxx>
#include <mmx/BlockHeader.hxx>
#include <mmx/ProofOfSpaceOG.hxx>
#include <mmx/ChainParams.hxx>
#include <mmx/secp256k1.hpp>
#include <vnx/vnx.h>
#include <fstream>
#include <optional>
using namespace mmx;
hash_t hash_value(uint64_t n){hash_t h;h.from_uint(n);return h;}
skey_t secret(uint32_t n){skey_t k;k[31]=n;return k;}
pubkey_t key(uint32_t n){return pubkey_t(secret(n));}
std::string wire(std::shared_ptr<const vnx::Value> value){std::vector<uint8_t>b;vnx::VectorOutputStream s(&b);vnx::TypeOutput out(&s);vnx::write(out,value);out.flush();return vnx::to_hex_string(b.data(),b.size());}
hash_t message(std::shared_ptr<const ValidatorVote>v){return hash_t("MMX/validator/vote/"+v->hash+v->farmer_key);}
std::shared_ptr<ValidatorVote> signed_vote(const hash_t& hash,uint32_t n){auto v=ValidatorVote::create();v->hash=hash;v->farmer_key=key(n);v->farmer_sig=signature_t::sign(secret(n),message(v));v->content_hash=v->calc_content_hash();return v;}
namespace mmx {
struct VoteReference {
    struct fork_t {std::shared_ptr<BlockHeader> block;bool is_proof_verified=false;std::map<pubkey_t,bool> validators;uint32_t votes=0;int64_t recv_time_ms=0;};
    struct proof_data_t {hash_t hash;};
    std::shared_ptr<ChainParams> params=ChainParams::create();uint32_t max_history=1000;
    std::map<hash_t,std::shared_ptr<BlockHeader>> headers;
    std::map<hash_t,std::shared_ptr<fork_t>> forks;
    std::map<hash_t,std::set<hash_t>> proofs;
    std::vector<std::pair<std::shared_ptr<const ValidatorVote>,int64_t>> vote_queue;
    std::vector<std::string> accepted;
    int64_t now=0;bool is_synced=false;static constexpr int DEBUG=0,INFO=0,output_verified_votes=0;std::ostringstream sink;
    int64_t get_time_ms()const{return now;}
    std::ostream& log(int){return sink;}
    void publish(std::shared_ptr<const ValidatorVote> v,int){accepted.push_back(v->content_hash.to_string());}
    std::shared_ptr<fork_t> find_fork(const hash_t& h)const{auto i=forks.find(h);return i==forks.end()?nullptr:i->second;}
    std::optional<proof_data_t> find_best_proof(const hash_t& h)const{auto i=proofs.find(h);if(i==proofs.end()||i->second.empty())return {};return proof_data_t{*i->second.begin()};}
    std::shared_ptr<const BlockHeader> find_prev(std::shared_ptr<const BlockHeader> b,size_t distance=1)const{for(size_t i=0;b&&i<distance;++i){auto p=headers.find(b->prev);b=p==headers.end()?nullptr:p->second;}return b;}
    std::set<pubkey_t> get_validators(std::shared_ptr<const BlockHeader>)const;
    void verify_votes();
};
// MMX_VOTE_METHODS
}
template<class F>std::string outcome(F f){try{return f()?"1":"0";}catch(const std::exception&e){return e.what();}}
int main(int argc,char**argv){
    if(argc!=4)return 1;secp256k1_init();std::ofstream out(argv[2]);
    auto emit_crypto=[&](std::string name,std::shared_ptr<ValidatorVote> v){vnx::Object row;row["kind"]="crypto";row["name"]=name;row["wire"]=wire(v);row["message"]=message(v).to_string();row["content"]=v->calc_content_hash().to_string();row["valid"]=outcome([&]{return v->is_valid();});out<<vnx::to_string(row)<<'\n';};
    for(uint32_t n=1;n<=24;++n){auto v=signed_vote(hash_value(100+n),n);emit_crypto("valid"+std::to_string(n),v);
        auto c=std::make_shared<ValidatorVote>(*v);c->content_hash[0]^=1;emit_crypto("content"+std::to_string(n),c);
        c=std::make_shared<ValidatorVote>(*v);c->hash[0]^=1;c->content_hash=c->calc_content_hash();emit_crypto("message"+std::to_string(n),c);
        c=std::make_shared<ValidatorVote>(*v);c->farmer_key=key(n+1);c->content_hash=c->calc_content_hash();emit_crypto("key"+std::to_string(n),c);}
    auto base=signed_vote(hash_value(42),1);const bytes_t<32> order("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141");
    for(unsigned mode=0;mode<7;++mode)for(bool wrong_content:{false,true}){auto v=std::make_shared<ValidatorVote>(*base);
        if(mode==0)v->farmer_key=pubkey_t();if(mode==1)v->farmer_sig=signature_t();if(mode==2)for(unsigned i=0;i<32;++i)v->farmer_sig[i]=order[i];if(mode==3){unsigned borrow=0;for(int i=31;i>=0;--i){int x=int(order[i])-int(base->farmer_sig[32+i])-borrow;v->farmer_sig[32+i]=x;borrow=x<0;}}if(mode==4)v->farmer_sig[0]^=1;if(mode==5)v->farmer_sig=signature_t::sign(secret(1),v->hash);if(mode==6){v->farmer_sig=signature_t();v->farmer_key=pubkey_t();}
        v->content_hash=v->calc_content_hash();if(wrong_content)v->content_hash[0]^=1;emit_crypto("edge"+std::to_string(mode)+"_"+std::to_string(wrong_content),v);}
    std::ifstream in(argv[1]);std::string line;while(std::getline(in,line)){auto row=vnx::from_string<vnx::Object>(line);VoteReference r;const auto kind=row["kind"].to<std::string>();
        if(kind=="history"){r.params->commit_delay=row["delay"].to<uint32_t>();r.params->max_validators=row["max_validators"].to<uint32_t>();r.max_history=row["max_history"].to<uint32_t>();std::vector<std::string> headers;for(const auto& h:row["headers"].to<std::vector<vnx::Object>>()){auto b=BlockHeader::create();b->hash=hash_value(h["id"].to<uint64_t>());b->prev=hash_value(h["prev"].to<uint64_t>());b->height=h["height"].to<uint32_t>();for(auto n:h["keys"].to<std::vector<uint32_t>>()){auto p=ProofOfSpaceOG::create();p->farmer_key=key(n);b->proof.push_back(p);}r.headers[b->hash]=b;headers.push_back(wire(b));}auto b=r.headers.at(hash_value(row["tip"].to<uint64_t>()));std::vector<std::string> keys;for(auto k:r.get_validators(b))keys.push_back(k.to_string());row["wires"]=headers;row["validators"]=keys;}
        else {r.params->block_interval_ms=row["interval"].to<int64_t>();std::vector<vnx::Object> events;for(auto event:row["events"].to<std::vector<vnx::Object>>()){const auto type=event["type"].to<std::string>();
            if(type=="register"){auto b=BlockHeader::create();b->hash=hash_value(event["block"].to<uint64_t>());b->height=1;b->proof_hash=hash_value(event["proof"].to<uint64_t>());auto p=ProofOfSpaceOG::create();p->challenge=hash_value(event["challenge"].to<uint64_t>());b->proof={p};auto f=std::make_shared<VoteReference::fork_t>();f->block=b;f->is_proof_verified=event["verified"].to<bool>();for(auto n:event["validators"].to<std::vector<uint32_t>>())f->validators[key(n)]=false;if(r.forks.emplace(b->hash,f).second&&f->is_proof_verified)r.proofs[p->challenge].insert(b->proof_hash);}
            if(type=="verify"){auto f=r.forks.at(hash_value(event["block"].to<uint64_t>()));f->is_proof_verified=true;r.proofs[f->block->proof[0]->challenge].insert(f->block->proof_hash);}
            if(type=="proof")r.proofs[hash_value(event["challenge"].to<uint64_t>())].insert(hash_value(event["proof"].to<uint64_t>()));
            if(type=="vote"){r.now=event["at"].to<int64_t>();auto v=signed_vote(hash_value(event["block"].to<uint64_t>()),event["key"].to<uint32_t>());event["wire"]=wire(v);if(v->is_valid())r.vote_queue.emplace_back(v,r.now);}
            if(type=="advance")r.now=event["at"].to<int64_t>();r.accepted.clear();r.verify_votes();vnx::Object counts;for(const auto& f:r.forks)counts[f.first.to_string()]=f.second->votes;event["counts"]=counts;event["pending"]=uint64_t(r.vote_queue.size());event["accepted"]=r.accepted;events.push_back(event);}
            row["events"]=events;}
        out<<vnx::to_string(row)<<'\n';}
    std::ifstream chain(argv[3]);while(std::getline(chain,line)){auto row=vnx::from_string<vnx::Object>(line);auto kind=row["kind"].to<std::string>();if(kind!="chain"&&kind!="deep_fork")continue;for(const auto& step:row["steps"].to<std::vector<vnx::Object>>()){const auto bytes=vnx::from_hex_string(step["block"].to<std::string>());vnx::VectorInputStream stream(&bytes);vnx::TypeInput input(&stream);std::shared_ptr<BlockHeader> block;vnx::read(input,block);for(unsigned n:{1,2}){auto v=signed_vote(block->hash,n);vnx::Object vote;vote["kind"]="chain_vote";vote["block"]=block->hash.to_string();vote["key"]=n;vote["wire"]=wire(v);out<<vnx::to_string(vote)<<'\n';}}}
}
