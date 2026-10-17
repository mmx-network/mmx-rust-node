#include <mmx/Block.hxx>
#include <mmx/ProofOfSpaceOG.hxx>
#include <mmx/ProofResponse.hxx>
#include <mmx/secp256k1.hpp>
#include <vnx/vnx.h>
#include <fstream>
using namespace mmx;
std::string wire(std::shared_ptr<const vnx::Value> value){std::vector<uint8_t>b;vnx::VectorOutputStream s(&b);vnx::TypeOutput out(&s);vnx::write(out,value);out.flush();return vnx::to_hex_string(b.data(),b.size());}
template<class F>std::string outcome(F f){try{return f()?"1":"0";}catch(const std::exception&e){return e.what();}}
int main(int argc,char**argv){
 if(argc!=3)return 1;secp256k1_init();skey_t secret;secret[31]=1;
 std::ifstream in(argv[1]);std::ofstream out(argv[2]);std::string line;unsigned count=0;
 while(std::getline(in,line)){
  auto row=vnx::from_string<vnx::Object>(line);auto kind=row["kind"].to<std::string>();if(kind!="chain"&&kind!="deep_fork")continue;
  for(auto step:row["steps"].to<std::vector<vnx::Object>>()){
   auto raw=vnx::from_hex_string(step["block"].to<std::string>());vnx::VectorInputStream stream(&raw);vnx::TypeInput input(&stream);std::shared_ptr<Block> block;vnx::read(input,block);
   for(unsigned mode=0;mode<16;++mode){
    auto r=ProofResponse::create();auto p=std::make_shared<ProofOfSpaceOG>(*std::dynamic_pointer_cast<const ProofOfSpaceOG>(block->proof[0]));r->proof=p;r->vdf_height=block->vdf_height;
    if(mode==6)r->proof=nullptr;
    if(mode==7)p->plot_id[0]^=1;
    if(mode==8)p->ksize=0;
    if(mode==9)p->proof_xs.resize(257);
    if(mode==10)r->vdf_height=0;
    if(mode==11)r->vdf_height=uint32_t(-1);
    r->hash=r->calc_hash();r->farmer_sig=signature_t::sign(secret,r->hash);
    if(mode==1)r->hash[0]^=1;
    if(mode==3)r->farmer_sig[0]^=1;
    if(mode==4)r->harvester=std::string(1023,'a');
    if(mode==5)r->harvester=std::string(1024,'a');
    if(mode==12){r->farmer_addr=vnx::Hash64(uint64_t(12345));r->harvester=std::string("bad\xff\x00",5);r->lookup_time_ms=-1234;}
    if(mode==13)r->farmer_sig=signature_t();
    if(mode==14)p->farmer_key=pubkey_t();
    if(mode==15){auto second=secret;second[31]=2;p->farmer_key=pubkey_t(second);r->hash=r->calc_hash();}
    r->content_hash=r->calc_content_hash();if(mode==2)r->content_hash[0]^=1;
    vnx::Object v;v["name"]=row["name"].to<std::string>()+std::to_string(block->height)+"_"+std::to_string(mode);v["mode"]=mode;v["wire"]=wire(r);v["hash"]=r->calc_hash().to_string();v["content"]=r->calc_content_hash().to_string();v["valid"]=outcome([&]{return r->is_valid();});v["auth"]=outcome([&]{r->validate();return true;});out<<vnx::to_string(v)<<'\n';++count;
   }
  }
 }
 std::cout<<count<<" native response cases"<<std::endl;
}
