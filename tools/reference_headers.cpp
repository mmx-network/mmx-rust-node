#include <mmx/Block.hxx>
#include <mmx/ProofOfSpaceOG.hxx>
#include <mmx/ProofOfSpaceNFT.hxx>
#include <mmx/write_bytes.h>
#include <vnx/vnx.h>
#include <fstream>
#include <iomanip>
#include <sstream>
using namespace mmx;

namespace mmx {
struct HeaderCapture : BlockHeader {
    vnx::Hash64 actual_type;
    vnx::Hash64 get_type_hash() const override { return actual_type; }
    std::vector<uint8_t> capture(std::shared_ptr<const ChainParams> params) const;
};
// MMX_HEADER_BYTES
struct ContextReference {
    std::shared_ptr<ChainParams> params;
    std::shared_ptr<BlockHeader> previous;
    hash_t state_hash;
    std::shared_ptr<const BlockHeader> find_prev(std::shared_ptr<const BlockHeader> block) const {
        return previous && block->prev==previous->hash ? previous : nullptr;
    }
    void validate(std::shared_ptr<const BlockHeader>) const;
};
// MMX_HEADER_CONTEXT
}
std::string hex(const uint8_t* data,size_t n) {
    std::ostringstream out;out<<std::hex<<std::setfill('0');
    for(size_t i=0;i<n;++i)out<<std::setw(2)<<unsigned(data[i]);
    return out.str();
}
template<size_t N> std::string hx(const bytes_t<N>& value) { return hex(value.data(),N); }
std::string wire(std::shared_ptr<const vnx::Value> value) {
    std::vector<uint8_t> bytes;vnx::VectorOutputStream stream(&bytes);vnx::TypeOutput out(&stream);
    vnx::write(out,value);out.flush();return hex(bytes.data(),bytes.size());
}
template<class F> std::string attempt(F f) { try { return f(); } catch(const std::exception&) { return "ERR"; } }
template<class F> std::string outcome(F f) { try { f();return "OK"; } catch(const std::exception& e) { return e.what(); } }
hash_t addr(uint64_t n) { hash_t out;out.from_uint(n);return out; }
skey_t secret(unsigned n=1) { skey_t out;out[31]=n;return out; }
std::shared_ptr<ProofOfSpace> proof(bool nft) {
    std::shared_ptr<ProofOfSpace> out;
    if(nft) { auto p=ProofOfSpaceNFT::create();p->ksize=32;p->seed=addr(11);p->proof_xs={0,1,0x12345678,UINT32_MAX};p->contract=addr(12);out=p; }
    else { auto p=ProofOfSpaceOG::create();p->ksize=29;p->seed=addr(11);p->proof_xs={0,1,0x12345678,UINT32_MAX};out=p; }
    out->farmer_key=pubkey_t(secret());out->challenge=addr(13);out->difficulty=42;out->score=255;
    if(auto p=std::dynamic_pointer_cast<ProofOfSpaceOG>(out))p->plot_id=hash_t(std::string("MMX/PLOTID/OG")+bytes_t<1>(&p->ksize,1)+p->seed+p->farmer_key);
    if(auto p=std::dynamic_pointer_cast<ProofOfSpaceNFT>(out))p->plot_id=hash_t(std::string("MMX/PLOTID/NFT")+bytes_t<1>(&p->ksize,1)+p->seed+p->farmer_key+p->contract);
    return out;
}
auto copy_header(std::shared_ptr<BlockHeader> original) {
    std::shared_ptr<BlockHeader> out;
    if(auto block=std::dynamic_pointer_cast<Block>(original))out=std::make_shared<Block>(*block);
    else out=std::make_shared<BlockHeader>(*original);
    return out;
}
void seal(std::shared_ptr<BlockHeader> h,std::shared_ptr<ChainParams> p,bool sign=true) {
    h->hash=h->calc_hash(p);
    if(sign)h->farmer_sig=signature_t::sign(secret(),h->hash);else h->farmer_sig=nullptr;
    h->content_hash=h->calc_content_hash();
}
auto header(bool block,bool nft,unsigned height,std::shared_ptr<ChainParams> p) {
    std::shared_ptr<BlockHeader> h=block?std::static_pointer_cast<BlockHeader>(Block::create()):BlockHeader::create();
    h->height=height;h->prev=addr(700);h->support_flags=0x89abcdef;h->vdf_height=123;h->nonce=UINT64_MAX;
    h->time_stamp=1010000;h->time_diff=1000;h->space_diff=17;h->weight=(uint128(1)<<127)+123;h->total_weight=uint128(-1);
    h->vdf_count=1;h->vdf_iters=987654321;h->vdf_output=addr(21);h->vdf_reward_addr={addr(22)};h->vdf_reward_payout=addr(23);
    h->proof={proof(nft)};h->proof_hash=h->proof[0]->calc_proof_hash();h->proof_chain=addr(24);h->challenge=addr(25);
    h->is_space_fork=true;h->space_fork_len=48;h->space_fork_proofs=6;h->reward_amount=123;h->reward_addr=addr(26);
    if(nft)h->reward_contract=std::dynamic_pointer_cast<const ProofOfSpaceNFT>(h->proof[0])->contract;
    h->reward_account=addr(27);h->reward_vote=-1;h->reward_vote_sum=-123456;h->reward_vote_count=123456;
    h->base_reward=34567890123;h->project_addr=addr(28);h->static_cost=123;h->total_cost=456;h->tx_count=0;h->tx_fees=789;h->txfee_buffer=101112;h->tx_hash=addr(29);
    seal(h,p);return h;
}
int main(int argc,char**argv) {
    if(argc!=3)return 1;secp256k1_init();std::ofstream headers(argv[1]),proofs(argv[2]);size_t hc=0,pc=0;
    std::map<std::pair<std::string,std::string>,std::string> snapshots;
    auto emit=[&](std::string name,std::shared_ptr<BlockHeader> h,std::shared_ptr<ChainParams> p,bool null_proof=false) {
        auto previous=BlockHeader::create();previous->hash=addr(700);previous->height=h->height-1;previous->time_stamp=1000000;
        ContextReference ctx;ctx.params=p;ctx.previous=previous;ctx.state_hash=previous->hash;
        vnx::Object row;row["name"]=name;row["wire"]=wire(h);
        const auto snapshot=std::make_pair(wire(p),wire(previous));
        auto found=snapshots.find(snapshot);
        if(found==snapshots.end()){snapshots[snapshot]=name;row["params"]=snapshot.first;row["previous"]=snapshot.second;}
        else row["seed"]=found->second;
        row["hash"]=null_proof?std::string("SKIP"):attempt([&]{return hx(h->calc_hash(p));});
        row["serialized"]=null_proof?std::string("SKIP"):attempt([&]{HeaderCapture capture;static_cast<BlockHeader&>(capture)=*h;capture.actual_type=h->get_type_hash();const auto b=capture.capture(p);return hex(b.data(),b.size());});
        row["content_hash"]=hx(h->calc_content_hash());
        row["valid"]=attempt([&]{return std::string(h->BlockHeader::is_valid(p)?"1":"0");});
        row["auth"]=(null_proof&&!h->proof[0])?std::string("SKIP"):outcome([&]{h->validate();});
        row["context"]=outcome([&]{ctx.validate(h);});headers<<vnx::to_string(row)<<'\n';++hc;
    };
    for(bool block:{false,true})for(bool nft:{false,true})for(unsigned height:{0,9,10,11}) {
        auto p=ChainParams::create();p->hardfork2_height=10;auto original=header(block,nft,height,p);
        const auto label=std::string(block?"block":"header")+(nft?"_nft_h":"_og_h")+std::to_string(height);
        emit(label,original,p);
        if(block)emit(label+"_sliced_header",std::make_shared<BlockHeader>(*original->get_header()),p);
        auto change=[&](std::string name,auto edit,bool reseal=true) {auto h=copy_header(original);edit(*h);if(reseal)seal(h,p);emit(label+"_"+name,h,p);};
        change("version",[](auto& h){h.version=1;});
        change("flags",[](auto& h){h.support_flags^=1;},false);
        change("hash",[](auto& h){h.hash=addr(1);},false);
        change("content",[](auto& h){h.content_hash=addr(1);},false);
        change("parent",[](auto& h){h.prev=addr(1);});
        change("negative_timestamp",[](auto& h){h.time_stamp=-1;});
        for(int64_t delta:{4999,5000,5001,19999,20000,20001})change("timestamp_"+std::to_string(delta),[&](auto& h){h.time_stamp=1000000+delta;});
        change("time_diff_low",[](auto& h){h.time_diff=999;});
        change("space_diff_zero",[](auto& h){h.space_diff=0;});
        change("fork_limit",[](auto& h){h.space_fork_len=4800;});
        change("fork_overflow",[](auto& h){h.space_fork_len=4801;});
        change("proof_chain",[](auto& h){h.proof_chain=addr(1);},false);
        change("weight",[](auto& h){h.weight^=uint128(1)<<127;},false);
        change("reward_vote",[](auto& h){h.reward_vote=1;},false);
        change("reward_without_address",[](auto& h){h.reward_addr=nullptr;});
        change("vdf_count_zero",[](auto& h){h.vdf_count=0;h.vdf_reward_addr.clear();});
        change("vdf_count_mismatch",[](auto& h){h.vdf_reward_addr.clear();});
        change("missing_signature",[](auto& h){h.farmer_sig=nullptr;h.content_hash=h.calc_content_hash();},false);
        change("zero_signature",[](auto& h){h.farmer_sig=signature_t();h.content_hash=h.calc_content_hash();},false);
        change("overflow_signature",[](auto& h){signature_t sig;for(unsigned i=0;i<32;++i)sig[i]=255;h.farmer_sig=sig;h.content_hash=h.calc_content_hash();},false);
        auto h=copy_header(original);h->proof.clear();seal(h,p);emit(label+"_empty_proofs",h,p);
        h=copy_header(original);h->proof={original->proof[0],nullptr};emit(label+"_null_proof",h,p,true);
        h=copy_header(original);h->proof={original->proof[0],original->proof[0]};seal(h,p);emit(label+"_duplicate_proofs",h,p);
        h=copy_header(original);auto second=proof(!nft);second->farmer_key=pubkey_t(secret(2));h->proof.push_back(second);seal(h,p);emit(label+"_second_farmer",h,p);
        for(unsigned kind:{0,1}) {
            h=copy_header(original);auto bad=proof(nft);bad->farmer_key=kind?pubkey_t(secret(2)):pubkey_t();h->proof={bad};seal(h,p);emit(label+"_key_"+std::to_string(kind),h,p);
        }
        for(unsigned kind:{0,1,2}) {
            h=copy_header(original);auto bad=proof(nft);
            if(auto q=std::dynamic_pointer_cast<ProofOfSpaceOG>(bad)){if(kind==0)q->ksize=0;if(kind==1)q->proof_xs.resize(257);}
            if(auto q=std::dynamic_pointer_cast<ProofOfSpaceNFT>(bad)){if(kind==0)q->ksize=0;if(kind==1)q->proof_xs.resize(257);}
            if(kind==2)bad->plot_id=addr(999);h->proof={bad};seal(h,p);emit(label+"_proof_shape_"+std::to_string(kind),h,p);
        }
    }
    for(bool block:{false,true}) {
        auto p=ChainParams::create();p->hardfork2_height=10;auto h=header(block,false,0,p);
        h->proof.clear();h->vdf_count=0;h->vdf_reward_addr.clear();h->reward_amount=0;h->reward_addr=nullptr;seal(h,p,false);
        emit(block?"block_genesis_unsigned":"header_genesis_unsigned",h,p);
    }
    for(bool nft:{false,true})for(unsigned ksize:{0,1,29,32,255})for(unsigned n:{0,1,2,256,257}) {
        auto p=proof(nft);
        if(auto q=std::dynamic_pointer_cast<ProofOfSpaceOG>(p)){q->ksize=ksize;q->proof_xs.clear();for(unsigned i=0;i<n;++i)q->proof_xs.push_back(i*0xabcdef01u);q->plot_id=hash_t(std::string("MMX/PLOTID/OG")+bytes_t<1>(&q->ksize,1)+q->seed+q->farmer_key);}
        if(auto q=std::dynamic_pointer_cast<ProofOfSpaceNFT>(p)){q->ksize=ksize;q->proof_xs.clear();for(unsigned i=0;i<n;++i)q->proof_xs.push_back(i*0xabcdef01u);q->plot_id=hash_t(std::string("MMX/PLOTID/NFT")+bytes_t<1>(&q->ksize,1)+q->seed+q->farmer_key+q->contract);}
        for(bool corrupt:{false,true}) {
            auto expected=p->plot_id;if(corrupt)p->plot_id=addr(999);
            vnx::Object row;row["wire"]=wire(p);row["hash"]=hx(p->calc_hash());row["proof_hash"]=hx(p->calc_proof_hash());row["plot_id"]=hx(expected);row["valid"]=bool(p->is_valid());row["identity"]=outcome([&]{p->validate();});proofs<<vnx::to_string(row)<<'\n';++pc;
        }
    }
    std::cout<<"Generated "<<hc<<" native header cases and "<<pc<<" native proof record cases\n";
    return headers&&proofs?0:1;
}
