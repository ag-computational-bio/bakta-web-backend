module github.com/ag-computational-bio/bakta-web-backend

go 1.16

require (
	github.com/ag-computational-bio/bakta-web-api-go v0.0.0-20210303185402-09d4bb672f38 // indirect
	github.com/aws/aws-sdk-go-v2 v1.2.0
	github.com/aws/aws-sdk-go-v2/config v1.1.1
	github.com/aws/aws-sdk-go-v2/service/s3 v1.2.0
	github.com/google/uuid v1.2.0
	github.com/jessevdk/go-flags v1.4.0
	github.com/spf13/viper v1.7.1
	golang.org/x/sync v0.0.0-20210220032951-036812b2e83c
	google.golang.org/genproto v0.0.0-20210303154014-9728d6b83eeb // indirect
	google.golang.org/grpc v1.36.0
	google.golang.org/protobuf v1.25.0
	gorm.io/driver/postgres v1.0.8
	gorm.io/driver/sqlite v1.1.4
	gorm.io/gorm v1.20.12
	k8s.io/api v0.20.4
	k8s.io/apimachinery v0.20.4
	k8s.io/client-go v0.20.4
)
