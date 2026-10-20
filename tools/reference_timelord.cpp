// TimeLord::handle/start/stop/update/compute are injected verbatim by the
// generator. Only the thread/event-loop adapter is replaced for deterministic
// boundary stepping. No ledger, plots or running services are accessed.
#include <mmx/IntervalRequest.hxx>
#include <mmx/ProofOfTime.hxx>
#include <mmx/helpers.h>
#include <mmx/utils.h>
#include <sha256_ni.h>
#include <sha256_arm.h>
#include <vnx/vnx.h>
#include <condition_variable>
#include <fstream>
#include <functional>
#include <iomanip>
#include <mutex>
#include <sstream>
#define log(level) std::ostringstream()
namespace mmx {
struct Reference {
    struct vdf_point_t { hash_t output; uint64_t num_iters=0; };
    std::mutex mutex;
    std::condition_variable vdf_signal;
    bool do_run=false,is_reset=false;
    uint64_t segment_iters=2,peak_iters=0,max_history=32;
    std::map<uint64_t,hash_t> history,infuse;
    std::map<uint64_t,std::shared_ptr<const IntervalRequest>> pending;
    std::shared_ptr<vdf_point_t> peak;
    skey_t timelord_sk;
    pubkey_t timelord_key;
    vnx::optional<addr_t> reward_addr;
    std::string output_proofs;
    std::vector<std::function<void()>> tasks;
    std::vector<std::shared_ptr<ProofOfTime>> proofs;
    vdf_point_t point;
    Reference() { timelord_sk[31]=1;timelord_key=pubkey_t(timelord_sk);hash_t reward;reward[0]=7;reward_addr=addr_t(reward); }
    void start_vdf(vdf_point_t);
    void stop_vdf();
    void handle(std::shared_ptr<const IntervalRequest>);
    void update();
    static hash_t compute(const hash_t&,uint64_t);
    void add_task(std::function<void()> f) { tasks.push_back(f); }
    void publish(std::shared_ptr<ProofOfTime> p,const std::string&) { proofs.push_back(p); }
    void settle() {
        if(!do_run)return;
        if(is_reset) { point=*peak;history.clear();is_reset=false; }
        else *peak=point;
        history[point.num_iters]=point.output;
        if(pending.count(point.num_iters))add_task(std::bind(&Reference::update,this));
        auto queue=std::move(tasks);tasks.clear();for(auto f:queue)f();
    }
    void advance(unsigned count) {
        for(unsigned i=0;i<count;++i) {
            settle();auto value=point.output;
            if(auto it=infuse.find(point.num_iters);it!=infuse.end()) { value=hash_t(value+it->second);value=hash_t(value+*reward_addr); }
            point.output=compute(value,segment_iters);point.num_iters+=segment_iters;settle();
        }
    }
};
// MMX_TIMELORD_METHODS
}
template<class T>std::string wire(const T& value) {
    std::vector<uint8_t> bytes;vnx::VectorOutputStream stream(&bytes);vnx::TypeOutput out(&stream);vnx::write(out,value);out.flush();
    return vnx::to_hex_string(bytes.data(),bytes.size());
}
mmx::hash_t hash(unsigned value) { mmx::hash_t h;h[0]=value;return h; }
int main(int argc,char** argv) {
    mmx::secp256k1_init();std::ofstream out(argv[1]);
    auto scenario=[&](const std::string& name,const std::vector<std::array<int64_t,6>>& commands) {
        mmx::Reference ref;vnx::Object row;row["name"]=name;std::vector<vnx::Object> events;
        for(const auto& command:commands) {
            vnx::Object event;
            switch(command[0]) {
            case 0: { auto req=mmx::IntervalRequest::create();req->vdf_height=command[1];req->start=command[2];req->end=command[3];req->infuse=hash(command[4]);if(command[5]>=0)req->input=hash(command[5]);event["submit"]=wire(req);ref.handle(req);ref.settle();break; }
            case 1: event["advance"]=command[1];ref.advance(command[1]);break;
            case 2: event["refresh"]=true;ref.peak_iters=0;break;
            case 3: event["stop"]=true;ref.stop_vdf();break;
            }
            std::vector<std::string> proofs;for(auto p:ref.proofs)proofs.push_back(wire(p));ref.proofs.clear();event["proofs"]=proofs;
            events.push_back(event);
        }
        row["events"]=events;
        std::vector<uint8_t> key;vnx::VectorOutputStream stream(&key);vnx::TypeOutput writer(&stream);vnx::write_generic(writer,ref.timelord_sk);writer.flush();row["key_file"]=vnx::to_hex_string(key.data(),key.size());
        out<<vnx::to_string(row)<<'\n';
    };
    scenario("duplicate_retry_forecast",{{0,1,0,8,10,1},{1,4,0,0,0,0},{0,1,0,8,10,1},{2,0,0,0,0,0},{0,1,0,8,10,1},{0,2,8,16,20,-1},{1,4,0,0,0,0}});
    scenario("changed_past_infusion",{{0,1,0,8,10,1},{0,2,8,16,20,-1},{1,8,0,0,0,0},{0,2,8,16,21,-1},{1,4,0,0,0,0}});
    scenario("faster_network_and_fork",{{0,1,0,8,10,1},{1,2,0,0,0,0},{0,8,100,108,30,99},{1,4,0,0,0,0},{0,8,100,108,40,99},{1,4,0,0,0,0}});
    scenario("stop_resume",{{0,1,0,8,10,1},{1,4,0,0,0,0},{3,0,0,0,0,0},{0,20,200,208,40,42},{1,4,0,0,0,0}});
    scenario("remove_intermediate_forecasts",{{0,2,4,8,20,-1},{0,3,8,12,30,-1},{0,1,0,12,10,1},{1,6,0,0,0,0}});
    mmx::secp256k1_free();
}
