#pragma once

#include <memory>
#include <string>
#include <vector>

#define D_ASSERT(x) ((void)0)

namespace spurtest {

class CatalogEntry;

namespace common {

template <typename T>
using shared_ptr = std::shared_ptr<T>;

}  // namespace common

class Catalog {
 public:
    Catalog();
    ~Catalog();

    bool Initialize(const std::string& path);
    CatalogEntry* GetEntry(const std::string& name) const;

    template <typename T>
    common::shared_ptr<T> Make() const;

    bool operator==(const Catalog& other) const;

 private:
    std::vector<common::shared_ptr<CatalogEntry>> entries_;
};

class CatalogEntry {
 public:
    CatalogEntry(std::string name) : name_(std::move(name)) {}
    const std::string& name() const { return name_; }

 private:
    std::string name_;
};

using CatalogPtr = common::shared_ptr<Catalog>;

}  // namespace spurtest
