#include "include/catalog.hpp"

#include <utility>

namespace spurtest {

Catalog::Catalog() = default;
Catalog::~Catalog() = default;

bool Catalog::Initialize(const std::string& path) {
    D_ASSERT(!path.empty());
    entries_.clear();
    return true;
}

CatalogEntry* Catalog::GetEntry(const std::string& name) const {
    for (const auto& entry : entries_) {
        if (entry->name() == name) {
            return entry.get();
        }
    }
    return nullptr;
}

bool Catalog::operator==(const Catalog& other) const {
    return entries_.size() == other.entries_.size();
}

}  // namespace spurtest
