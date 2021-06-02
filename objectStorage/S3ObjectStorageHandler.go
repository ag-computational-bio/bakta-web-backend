package objectStorage

import (
	"context"
	"log"
	"path"
	"reflect"
	"strings"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/config"
	"github.com/aws/aws-sdk-go-v2/service/s3"
)

type S3ObjectStorageHandler struct {
	S3Client      *s3.Client
	PresignClient *s3.PresignClient
	S3Endpoint    string
}

type UploadLinks struct {
	TSV             string `bakta:"tsv"`
	GFF3            string `bakta:"gff3"`
	GBFF            string `bakta:"gbff"`
	FNA             string `bakta:"fna"`
	FAA             string `bakta:"faa"`
	JSON            string `bakta:"json"`
	EMBL            string `bakta:"embl"`
	TSVHypothetical string `bakta:"hypotheticals.tsv"`
	FAAHypothetical string `bakta:"hypotheticals.faa"`
}

func InitS3ObjectStorageHandler(bucket string) (*S3ObjectStorageHandler, error) {
	endpoint := "https://s3.computational.bio.uni-giessen.de"

	cfg, err := config.LoadDefaultConfig(
		context.Background(),
		config.WithRegion("RegionOne"),
		config.WithEndpointResolver(aws.EndpointResolverFunc(
			func(service, region string) (aws.Endpoint, error) {
				return aws.Endpoint{
					URL: endpoint,
				}, nil
			})),
	)

	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	client := s3.NewFromConfig(cfg)

	presignClient := s3.NewPresignClient(client)

	handler := S3ObjectStorageHandler{
		S3Client:      client,
		PresignClient: presignClient,
		S3Endpoint:    endpoint,
	}

	return &handler, nil
}

func (handler *S3ObjectStorageHandler) CreateUploadLink(bucket string, key string) (string, error) {

	presignedRequestURL, err := handler.PresignClient.PresignPutObject(context.Background(), &s3.PutObjectInput{
		Bucket:  aws.String(bucket),
		Key:     aws.String(key),
		Expires: aws.Time(time.Now().AddDate(0, 0, 11)),
	})

	if err != nil {
		log.Println(err.Error())
		return "", err
	}

	return presignedRequestURL.URL, nil
}

func (handler *S3ObjectStorageHandler) CreateDownloadLinks(bucket string, key string, prefix string) (*UploadLinks, error) {
	uploadLinks := UploadLinks{}

	uploadStructType := reflect.TypeOf(UploadLinks{})
	uploadStructValue := reflect.ValueOf(&uploadLinks)

	for i := 0; i < uploadStructType.NumField(); i++ {
		fieldFileSuffix := uploadStructType.Field(i).Tag.Get("bakta")
		fullFilename := strings.Join([]string{"result", ".", fieldFileSuffix}, "")

		keyWithFilename := path.Join(key, fullFilename)

		presignedRequestURL, err := handler.PresignClient.PresignGetObject(context.Background(), &s3.GetObjectInput{
			Bucket: aws.String(bucket),
			Key:    aws.String(keyWithFilename),
		})

		if err != nil {
			log.Println(err.Error())
			return nil, err
		}

		uploadStructValueElem := uploadStructValue.Elem()
		uploadStructValueElem.Field(i).SetString(presignedRequestURL.URL)
	}

	return &uploadLinks, nil
}
