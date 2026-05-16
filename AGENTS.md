# Agents Guide

## 编译与测试

### 快速命令

```bash
# 编译 Release 版本
make build

# 运行所有测试
make test

# 清理构建产物
make clean
```

### 分项命令

| 命令 | 说明 |
|------|------|
| `make build` | 编译 release 版本二进制到 `target/release/dscode` |
| `make check` | 运行 `cargo check` 类型检查 |
| `make test` | 运行 `cargo check` + `cargo test` |

### CI 推荐流程

```bash
make test
```
