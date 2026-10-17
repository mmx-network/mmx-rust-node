#include <mmx/BlockHeader.hxx>
#include <mmx/VDF_Point.hxx>
#include <mmx/ChainParams.hxx>
#include <vnx/vnx.h>
#include <fstream>
using namespace mmx;
hash_t hash_value(uint64_t n){hash_t h;h.from_uint(n);return h;}
namespace mmx {
struct NextReference {
 std::shared_ptr<ChainParams> params=ChainParams::create();uint64_t num=1;
 std::multimap<uint64_t,std::shared_ptr<const VDF_Point>> vdf_index;
 hash_t get_infusion(std::shared_ptr<const BlockHeader>b,uint32_t,uint64_t& n)const{n=num;return b->hash;}
 std::vector<std::shared_ptr<const VDF_Point>> find_next_vdf_points(std::shared_ptr<const BlockHeader>)const;
};
// MMX_NEXT_VDF
}
int main(int argc,char**argv){
 if(argc!=3)return 1;std::ifstream in(argv[1]);std::ofstream out(argv[2]);std::string text;
 while(std::getline(in,text)){
  auto row=vnx::from_string<vnx::Object>(text);NextReference r;r.num=row["num"].to<uint64_t>();r.params->max_vdf_count=row["maximum"].to<uint32_t>();
  auto peak=BlockHeader::create();peak->hash=hash_value(41);peak->vdf_iters=std::stoull(row["iters"].to<std::string>());
  for(auto v:row["points"].to<std::vector<vnx::Object>>()){
   auto p=VDF_Point::create();p->start=std::stoull(v["start"].to<std::string>());p->num_iters=v["num"].to<uint64_t>();p->input=hash_value(v["input"].to<uint64_t>());p->output=hash_value(v["output"].to<uint64_t>());p->prev=hash_value(v["prev"].to<uint64_t>());p->recv_time=v["recv_time"].to<int64_t>();r.vdf_index.emplace(p->start+p->num_iters,p);
  }
  std::vector<std::string> outputs;for(auto p:r.find_next_vdf_points(peak))outputs.push_back(p->output.to_string());row["outputs"]=outputs;out<<vnx::to_string(row)<<'\n';
 }
}
